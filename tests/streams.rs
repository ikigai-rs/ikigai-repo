use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Body, Capability, Error, Iri, Kernel, Request, Resolution, Scope, Space, Verb,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::current_dir()
            .unwrap()
            .join("target/stream-fixture");
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "second \"quoted\" 🦀",
            ],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn log_body_is_incremental_and_matches_bytes_in_both_faces() {
    let scratch = Scratch::new();
    let space = Arc::new(ikigai_repo::space_with_roots([&scratch.0]));
    let kernel = Kernel::new(space.clone());
    let root = Capability::root();
    for json in [false, true] {
        let mut request = Request::new(Verb::Source, Iri::parse("urn:repo:log").unwrap()).with_arg(
            "dir",
            ArgRef::Inline(scratch.0.to_str().unwrap().as_bytes().to_vec()),
        );
        if json {
            request = request.with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        }
        // invoke() is the preserved bytes implementation; Kernel::issue uses
        // invoke_body and collects, so comparing only the two kernel APIs
        // would compare the new implementation with itself.
        let Resolution::Hit(resolved) = space.resolve(&request, &Scope::empty()) else {
            panic!("log resolves")
        };
        let inv = ikigai_core::Invocation::detached(&request, &resolved.bindings, &root);
        let expected = block_on(resolved.endpoint.invoke(&inv)).unwrap();
        let answer = block_on(kernel.issue_body(request.clone(), &root)).unwrap();
        assert!(
            matches!(answer.bytes, Body::Once(_)),
            "log buffered its entire answer"
        );
        assert_eq!(answer.repr_type, expected.repr_type);
        assert_eq!(
            block_on(answer.bytes.collect(100_000)).unwrap(),
            expected.bytes
        );

        let tiny = Kernel::new(space.clone()).with_stream_memory(1, 1);
        let answer = block_on(tiny.issue_body(request, &root)).unwrap();
        assert!(matches!(
            block_on(answer.bytes.collect(100_000)),
            Err(Error::OverBudget { .. })
        ));
    }
}

#[test]
fn body_path_preserves_authority_and_argument_refusals() {
    let kernel = Kernel::new(Arc::new(ikigai_repo::space()));
    for (iri, pr) in [("urn:repo:log", false), ("urn:repo:pr:diff", true)] {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        if pr {
            request = request.with_arg("pr", ArgRef::Inline(b"1".to_vec()));
        }
        let result =
            block_on(kernel.issue_body(request.clone(), &Capability::scoped(Vec::<String>::new())));
        assert!(matches!(result, Err(Error::Denied(_))));
        let request = request.with_arg("dir", ArgRef::Inline(b"/outside-roots".to_vec()));
        assert!(matches!(
            block_on(kernel.issue_body(request, &Capability::root())),
            Err(Error::Denied(_))
        ));
    }
    let request = Request::new(Verb::Source, Iri::parse("urn:repo:pr:diff").unwrap());
    assert!(matches!(
        block_on(kernel.issue_body(request, &Capability::root())),
        Err(Error::MissingArgument(_))
    ));
}

#[cfg(unix)]
#[test]
fn pr_diff_body_matches_bytes_without_reaching_github() {
    const MARKER: &str = "IKIGAI_REPO_DIFF_FIXTURE";
    if std::env::var_os(MARKER).is_none() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::current_dir()
            .unwrap()
            .join("target/gh-stream-fixture");
        std::fs::create_dir_all(&base).unwrap();
        let gh = base.join("gh");
        std::fs::write(&gh, "#!/bin/sh\n[ \"$*\" = 'pr diff 42 --repo owner/name' ] || exit 9\nprintf 'diff --git a/file b/file\\n+hello \\377\\n'\n").unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut paths = vec![base.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        // Isolate PATH in a child test process: other tests cannot accidentally
        // use the fake gh, and this fixture never contacts GitHub.
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pr_diff_body_matches_bytes_without_reaching_github",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("PATH", std::env::join_paths(paths).unwrap())
            .status()
            .unwrap();
        std::fs::remove_dir_all(base).unwrap();
        assert!(status.success());
        return;
    }
    let space = Arc::new(ikigai_repo::space());
    let kernel = Kernel::new(space.clone());
    let cap = Capability::scoped(["urn:cap:exec:gh"]);
    let request = Request::new(Verb::Source, Iri::parse("urn:repo:pr:diff").unwrap())
        .with_arg("pr", ArgRef::Inline(b"42".to_vec()))
        .with_arg("repo", ArgRef::Inline(b"owner/name".to_vec()));
    let Resolution::Hit(resolved) = space.resolve(&request, &Scope::empty()) else {
        panic!("diff resolves")
    };
    let inv = ikigai_core::Invocation::detached(&request, &resolved.bindings, &cap);
    let expected = block_on(resolved.endpoint.invoke(&inv)).unwrap();
    let body = block_on(kernel.issue_body(request, &cap)).unwrap();
    assert!(matches!(body.bytes, Body::Once(_)));
    assert_eq!(body.repr_type, expected.repr_type);
    assert_eq!(block_on(body.bytes.collect(1000)).unwrap(), expected.bytes);
}
