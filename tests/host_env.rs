//! What the host's own environment can make a facade do. This binary holds ONE
//! test because it sets process environment variables, which every thread of a
//! test binary shares. Fixtures are fresh repositories under `target/host-env`;
//! gh is pointed only at a loopback listener, never at a real host.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
}

fn repo(path: &Path, subject: &str) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    git(path, &["commit", "-q", "--allow-empty", "-m", subject]);
}

fn issue(
    kernel: &Kernel,
    iri: &str,
    args: &[(&str, &str)],
    cap: &Capability,
    body: bool,
) -> ikigai_core::Result<Vec<u8>> {
    let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
    for (key, value) in args {
        request = request.with_arg(*key, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    if body {
        let answer = block_on(kernel.issue_body(request, cap))?;
        Ok(block_on(answer.bytes.collect(1 << 20))?.to_vec())
    } else {
        Ok(block_on(kernel.issue(request, cap))?.bytes.to_vec())
    }
}

#[test]
fn the_host_environment_neither_redirects_nor_leaks() {
    let base: PathBuf = std::env::current_dir().unwrap().join("target/host-env");
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    let inside = root.join("inside");
    let outside = base.join("outside");
    repo(&inside, "inside-subject");
    repo(&outside, "outside-subject");
    let marker = base.join("token-seen-by-git");
    git(
        &inside,
        &[
            "config",
            "core.fsmonitor",
            &format!("echo \"${{GH_TOKEN:-unset}}\" > {}; false", marker.display()),
        ],
    );
    let gh_config = base.join("gh-config");
    std::fs::create_dir_all(&gh_config).unwrap();

    // A host started from a git hook or a shell with GIT_DIR exported, holding
    // GitHub tokens for gh. Edition 2021: set_var is safe, and this binary's
    // only test is this one.
    std::env::set_var("GIT_DIR", outside.join(".git"));
    std::env::set_var("GH_TOKEN", "probe-github-token");
    std::env::set_var("GH_ENTERPRISE_TOKEN", "probe-enterprise-token");
    std::env::set_var("GH_CONFIG_DIR", &gh_config);
    std::env::set_var("GH_NO_UPDATE_NOTIFIER", "1");
    std::env::set_var("GH_PROMPT_DISABLED", "1");

    let kernel = Kernel::new(Arc::new(ikigai_repo::space_with_roots([&root])));
    let all = Capability::root();
    let dir = inside.to_str().unwrap();

    // dir= names the repository; an inherited GIT_DIR must not replace it.
    for body in [false, true] {
        let log = issue(&kernel, "urn:repo:log", &[("dir", dir)], &all, body).unwrap();
        let log = String::from_utf8_lossy(&log);
        assert!(log.contains("inside-subject"), "body={body}: {log}");
        assert!(!log.contains("outside-subject"), "body={body}: {log}");
    }

    // A git read needs no GitHub token, so code a repository's configuration
    // runs (here a trusted repository's own core.fsmonitor) is not handed one.
    issue(&kernel, "urn:repo:status", &[("dir", dir)], &all, false).unwrap();
    let seen = std::fs::read_to_string(&marker).expect("core.fsmonitor ran");
    assert_eq!(seen.trim(), "unset", "git's children saw GH_TOKEN");

    // repo= must not choose where gh connects, nor which token it sends there.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let connected = Arc::new(AtomicBool::new(false));
    let seen = connected.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            seen.store(true, Ordering::SeqCst);
            // Read the client's first bytes and hang up, so gh fails fast.
            if let Ok(mut stream) = stream {
                let _ = stream.read(&mut [0; 64]);
            }
        }
    });
    let repo = format!("127.0.0.1:{port}/owner/name");
    for (iri, body) in [
        ("urn:repo:pr:view", false),
        ("urn:repo:pr:diff", true),
        ("urn:repo:pr:list", false),
        ("urn:repo:pr:files", false),
    ] {
        let out = issue(&kernel, iri, &[("pr", "1"), ("repo", &repo)], &all, body);
        assert!(out.is_err(), "{iri}: {out:?}");
        assert!(
            !connected.load(Ordering::SeqCst),
            "{iri} connected to the caller's host: {out:?}"
        );
    }
}
