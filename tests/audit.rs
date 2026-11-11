//! Audit fixtures use only fresh repositories under this checkout's target directory.
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Invocation, Iri, Kernel, Request, Resolution, Scope, Space, Verb,
};
use std::{path::PathBuf, process::Command, sync::Arc};

struct Repo(PathBuf);
impl Repo {
    fn new(name: &str) -> Self {
        let path = std::env::current_dir()
            .unwrap()
            .join("target/audit")
            .join(name);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .trim_end_matches('\n')
            .into()
    }
    fn read(
        &self,
        iri: &str,
        args: &[(&str, &str)],
    ) -> ikigai_core::Result<ikigai_core::Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap()).with_arg(
            "dir",
            ArgRef::Inline(self.0.to_str().unwrap().as_bytes().to_vec()),
        );
        for (key, value) in args {
            request = request.with_arg(*key, ArgRef::Inline(value.as_bytes().to_vec()));
        }
        block_on(
            Kernel::new(Arc::new(ikigai_repo::space_with_roots([self
                .0
                .parent()
                .unwrap()])))
            .issue(request, &Capability::root()),
        )
    }
    fn log(&self, dir: &std::path::Path, path: Option<&str>, json: bool) -> Vec<u8> {
        let space = Arc::new(ikigai_repo::space_with_roots([&self.0]));
        let mut request = Request::new(Verb::Source, Iri::parse("urn:repo:log").unwrap()).with_arg(
            "dir",
            ArgRef::Inline(dir.to_str().unwrap().as_bytes().to_vec()),
        );
        if json {
            request = request.with_arg("as", ArgRef::Inline(b"application/json".to_vec()));
        }
        if let Some(path) = path {
            request = request.with_arg("path", ArgRef::Inline(path.as_bytes().to_vec()));
        }
        let cap = Capability::root();
        let Resolution::Hit(resolved) = space.resolve(&request, &Scope::empty()) else {
            panic!("log resolves")
        };
        let inv = Invocation::detached(&request, &resolved.bindings, &cap);
        let collected = block_on(resolved.endpoint.invoke(&inv)).unwrap().bytes;
        let body = block_on(Kernel::new(space).issue_body(request, &cap)).unwrap();
        let streamed = block_on(body.bytes.collect(1_000_000)).unwrap();
        assert_eq!(streamed, collected, "streamed and collected log differ");
        collected
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn log_json_preserves_separator_in_author_and_subject() {
    let repo = Repo::new("log-separator");
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "--author",
        "First\x1fLast <a@example.com>",
        "-m",
        "subject\x1fpart\n\nbody\nsecond line",
    ]);
    let hash = repo.git(&["rev-parse", "HEAD"]);
    let date = repo.git(&["log", "-1", "--format=%aI"]);
    let expected = format!("[{{\"hash\":\"{hash}\",\"author\":\"First\\u001fLast\",\"date\":\"{date}\",\"subject\":\"subject\\u001fpart\"}}]");
    assert_eq!(
        String::from_utf8(repo.log(&repo.0, None, true)).unwrap(),
        expected
    );
}

#[test]
fn log_path_is_relative_to_repository_root_from_a_subdirectory() {
    let repo = Repo::new("log-subdirectory");
    std::fs::create_dir(repo.0.join("sub")).unwrap();
    std::fs::write(repo.0.join("sub/file"), "content").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "touch sub/file"]);
    for json in [false, true] {
        for path in [
            "sub/file",
            ":(glob)sub/*",
            ":(literal)sub/file",
            ":/sub/file",
            ":!missing",
            ":^missing",
            ":sub/file",
            ":",
        ] {
            let expected = repo.log(&repo.0, Some(path), json);
            let actual = repo.log(&repo.0.join("sub"), Some(path), json);
            assert_eq!(actual, expected, "path scope changed with dir= for {path}");
            if !json {
                let direct = repo.git(&["log", "--oneline", "-20", "--", path]);
                assert_eq!(
                    String::from_utf8(actual).unwrap().trim_end_matches('\n'),
                    direct,
                    "pathspec meaning changed for {path}"
                );
            }
        }
    }
}

#[test]
fn log_json_handles_empty_and_large_multiline_messages_in_order() {
    let repo = Repo::new("log-messages");
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "--allow-empty-message",
        "-m",
        "",
    ]);
    let empty = repo.git(&["rev-parse", "HEAD"]);
    let subject = format!("{}\t\"\\\x1f🦀", "a".repeat(20_000));
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!("{subject}\n\nbody\nsecond body line"),
    ]);
    let newest = repo.git(&["rev-parse", "HEAD"]);
    let log = String::from_utf8(repo.log(&repo.0, None, true)).unwrap();
    assert_eq!(log.matches("\"hash\":").count(), 2);
    assert!(log.find(&newest).unwrap() < log.find(&empty).unwrap());
    assert!(log.contains("\"subject\":\"\""), "empty subject lost");
    assert!(log.contains(&format!("{}\\t\\\"\\\\\\u001f🦀", "a".repeat(20_000))));
    assert!(!log.contains("body"));
}

#[test]
fn show_and_object_preserve_unusual_paths_through_renames_and_copies() {
    let repo = Repo::new("paths");
    let paths = [
        "space name",
        "line\nbreak",
        "tab\tname",
        "quote\"name",
        "back\\slash",
        "-dash",
        "café🦀",
        "sub/file",
    ];
    for (i, path) in paths.iter().enumerate() {
        let file = repo.0.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, [format!("bytes {i}\0").as_bytes(), &[0xff]].concat()).unwrap();
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "paths"]);
    let original = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["mv", "space name", "renamed\nfile"]);
    std::fs::copy(repo.0.join("tab\tname"), repo.0.join("copied\tfile")).unwrap();
    repo.git(&["add", "."]);
    let staged = String::from_utf8(repo.read("urn:repo:status", &[]).unwrap().bytes).unwrap();
    assert!(staged.contains("R  "), "rename missing: {staged}");
    assert!(staged.contains("copied\\tfile"), "copy missing: {staged}");
    assert_eq!(
        staged.trim_end_matches('\n'),
        repo.git(&["status", "--porcelain=v1", "-b"])
    );
    repo.git(&["commit", "-q", "-m", "rename and copy"]);
    for path in paths
        .iter()
        .copied()
        .chain(["renamed\nfile", "copied\tfile"])
    {
        // A leading dash is explicitly refused; ./ is also outside show's contract.
        if path == "-dash" {
            assert!(matches!(
                repo.read("urn:repo:show", &[("rev", &original), ("path", path)]),
                Err(ikigai_core::Error::InvalidArgument { .. })
            ));
            continue;
        }
        let rev = if path == "space name" {
            &original
        } else {
            "HEAD"
        };
        let answer = repo
            .read("urn:repo:show", &[("rev", rev), ("path", path)])
            .unwrap();
        let expected_path = if path == "space name" {
            "renamed\nfile"
        } else {
            path
        };
        assert_eq!(
            answer.bytes,
            std::fs::read(repo.0.join(expected_path)).unwrap(),
            "{path:?}"
        );
    }
    // A renamed blob's provenance still names the original path.
    let blob = repo.git(&["rev-parse", "HEAD:renamed\nfile"]);
    let answer = repo
        .read(
            &format!("urn:repo:paths:object:sha:{blob}"),
            &[("as", "application/json")],
        )
        .unwrap();
    let json = String::from_utf8(answer.bytes).unwrap();
    assert!(json.contains(&original));
    assert!(json.contains("\"path\":\"space name\""));
    let status = repo.read("urn:repo:status", &[]).unwrap();
    assert_eq!(
        String::from_utf8(status.bytes)
            .unwrap()
            .trim_end_matches('\n'),
        repo.git(&["status", "--porcelain=v1", "-b"])
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_object_path_is_refused_and_status_uses_git_quoting() {
    use std::os::unix::ffi::OsStrExt;
    let repo = Repo::new("non-utf8");
    std::fs::write(
        repo.0.join(std::ffi::OsStr::from_bytes(b"bad\xff")),
        b"content",
    )
    .unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "non-UTF-8 path"]);
    let blob = repo.git(&["ls-tree", "--format=%(objectname)", "HEAD"]);
    let error = repo
        .read(&format!("urn:repo:non-utf8:object:sha:{blob}"), &[])
        .unwrap_err();
    assert!(
        matches!(error, ikigai_core::Error::Endpoint(ref s) if s.contains("object path is not UTF-8"))
    );
    std::fs::write(
        repo.0.join(std::ffi::OsStr::from_bytes(b"bad\xff")),
        b"changed",
    )
    .unwrap();
    let status = repo.read("urn:repo:status", &[]).unwrap();
    assert!(String::from_utf8(status.bytes)
        .unwrap()
        .contains("bad\\377"));
}

#[test]
fn empty_unborn_detached_and_conflicted_states_have_typed_answers() {
    let repo = Repo::new("states");
    let status = repo.read("urn:repo:status", &[]).unwrap();
    assert!(String::from_utf8(status.bytes)
        .unwrap()
        .starts_with("## No commits yet on "));
    let branch = repo.git(&["symbolic-ref", "--short", "HEAD"]);
    assert_eq!(
        String::from_utf8(repo.read("urn:repo:branch", &[]).unwrap().bytes)
            .unwrap()
            .trim(),
        branch
    );
    assert!(matches!(
        repo.read("urn:repo:log", &[]),
        Err(ikigai_core::Error::Endpoint(_))
    ));
    assert!(matches!(
        repo.read("urn:repo:show", &[("rev", "HEAD"), ("path", "file")]),
        Err(ikigai_core::Error::NotFound(_))
    ));
    std::fs::write(repo.0.join("file"), "base\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "base"]);
    let base = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "-q", "--detach"]);
    assert!(repo.read("urn:repo:branch", &[]).unwrap().bytes.is_empty());
    repo.log(&repo.0, None, true);
    repo.git(&["checkout", "-q", "-b", "side"]);
    std::fs::write(repo.0.join("file"), "side\n").unwrap();
    repo.git(&["commit", "-qam", "side"]);
    repo.git(&["checkout", "-q", &branch]);
    std::fs::write(repo.0.join("file"), "main\n").unwrap();
    repo.git(&["commit", "-qam", "main"]);
    let merge = Command::new("git")
        .arg("-C")
        .arg(&repo.0)
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "merge",
            "side",
        ])
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    assert!(repo.0.join(".git/MERGE_HEAD").exists());
    let status = String::from_utf8(repo.read("urn:repo:status", &[]).unwrap().bytes).unwrap();
    assert!(status.contains("UU file"), "{status}");
    assert_eq!(
        repo.read("urn:repo:show", &[("rev", &base), ("path", "file")])
            .unwrap()
            .bytes,
        b"base\n"
    );
    repo.log(&repo.0, None, true);
}

#[test]
fn qualified_refs_work_in_shallow_clones_and_worktrees() {
    let repo = Repo::new("refs");
    std::fs::write(repo.0.join("file"), "old").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-q", "-m", "old"]);
    repo.git(&["tag", "same"]);
    std::fs::write(repo.0.join("file"), "new").unwrap();
    repo.git(&["commit", "-qam", "new"]);
    repo.git(&["branch", "same"]);
    for (rev, expected) in [
        ("same", b"old".as_slice()),
        ("refs/tags/same", b"old"),
        ("refs/heads/same", b"new"),
    ] {
        assert_eq!(
            repo.read("urn:repo:show", &[("rev", rev), ("path", "file")])
                .unwrap()
                .bytes,
            expected
        );
    }
    let shallow = Repo(repo.0.with_extension("shallow"));
    repo.git(&[
        "clone",
        "-q",
        "--depth",
        "1",
        &format!("file://{}", repo.0.display()),
        shallow.0.to_str().unwrap(),
    ]);
    assert_eq!(
        shallow.git(&["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let log = String::from_utf8(shallow.log(&shallow.0, None, true)).unwrap();
    assert_eq!(log.matches("\"hash\":").count(), 1);
    assert_eq!(
        shallow
            .read("urn:repo:show", &[("rev", "HEAD"), ("path", "file")])
            .unwrap()
            .bytes,
        b"new"
    );
    let worktree = Repo(repo.0.with_extension("worktree"));
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.0.to_str().unwrap(),
    ]);
    assert!(worktree.0.join(".git").is_file());
    worktree.log(&worktree.0, None, true);
    assert_eq!(
        worktree
            .read("urn:repo:show", &[("rev", "HEAD"), ("path", "file")])
            .unwrap()
            .bytes,
        b"new"
    );
    assert!(worktree
        .read("urn:repo:branch", &[])
        .unwrap()
        .bytes
        .is_empty());
}

#[test]
fn submodule_is_reported_by_status_and_refused_as_a_file() {
    let child = Repo::new("submodule-child");
    child.git(&["commit", "-q", "--allow-empty", "-m", "child"]);
    let repo = Repo::new("submodule-parent");
    repo.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        child.0.to_str().unwrap(),
        "nested module",
    ]);
    repo.git(&["commit", "-q", "-m", "submodule"]);
    assert!(matches!(
        repo.read(
            "urn:repo:show",
            &[("rev", "HEAD"), ("path", "nested module")]
        ),
        Err(ikigai_core::Error::NotFound(_))
    ));
    std::fs::write(repo.0.join("nested module/untracked"), "dirty").unwrap();
    let status = String::from_utf8(repo.read("urn:repo:status", &[]).unwrap().bytes).unwrap();
    assert!(status.contains(" M \"nested module\""), "{status}");
    assert_eq!(
        status.trim_end_matches('\n'),
        repo.git(&["status", "--porcelain=v1", "-b"])
    );
}
