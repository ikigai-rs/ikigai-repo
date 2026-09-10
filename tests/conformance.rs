//! The module recipe as one test: `ikigai-conformance` walks the ten endpoints
//! [`ikigai_repo::space`] binds and reports every violation at once.
//!
//! ## Nothing here is pure, and nothing is cached
//!
//! Every endpoint reads a working tree, git state, a directory, or GitHub. None
//! marks its result `.cacheable()`, so every representation is live and the
//! suite's cache probe has nothing to hold it to — and no endpoint is declared
//! `pure` or `cacheable` here, so the day one is marked cacheable without a
//! golden thread it can be cut by, this test goes red.
//!
//! Why not cache a working-tree read under a thread? The host's workspace
//! watcher cuts `urn:file:<path>` per changed file, with the path RELATIVE to a
//! root the host configured — a root this module cannot see from an absolute
//! `dir=`, and there is no directory-level thread that "any file under `dir`
//! changed" could name. A thread this module minted would be a promise no host
//! keeps. So: when in doubt, don't cache.
//!
//! ## The fixture kernel: a hermetic scratch repository
//!
//! CI checkouts are shallow (fetch-depth 1), so no fired action reads the
//! enclosing repository. [`Scratch`] builds a fresh `git init` with one commit
//! under a hermetic identity, inside a base directory that doubles as the ROOT
//! `urn:repo:list` scans (so the listing finds exactly that one repo). Every
//! git-backed action takes `dir=` pointing at it; `urn:system:exec` runs
//! `git rev-parse HEAD` there.
//!
//! ## The GitHub facades are opted out — and their enforcement pinned by hand
//!
//! `urn:repo:pr:*` reach api.github.com through `gh`: not resolvable in CI (no
//! token) and live data anywhere. They are opted out of the invoking checks with
//! that reason; the static checks (ARGSPECS, NAMES, REQUIRES-VERB, PIPELINE)
//! still run on them. `Suite::opt_out` also drops ENFORCED for an action — the
//! one invoking check that never reaches the network, since the capability is
//! checked before a process is spawned — so
//! [`github_facades_are_denied_under_no_grants`] pins that half itself.
//!
//! No module namespace: there is no RDF face. NAMES runs: every id is kebab-case.

use ikigai_conformance::{Fixture, Report, Suite};
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Representation, Request, Verb};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// The git-backed facades, by description id: each takes `dir=`.
const GIT_FACADES: [&str; 3] = ["repo-status", "repo-log", "repo-branch"];

/// The GitHub-backed facades, by description id and bound IRI.
const GH_FACADES: [(&str, &str); 5] = [
    ("repo-pr-list", "urn:repo:pr:list"),
    ("repo-pr-view", "urn:repo:pr:view"),
    ("repo-pr-files", "urn:repo:pr:files"),
    ("repo-pr-diff", "urn:repo:pr:diff"),
    ("repo-pr-checks", "urn:repo:pr:checks"),
];

/// Why the GitHub facades are not fired: `gh` calls the API for every one of
/// them, which CI cannot do and which is never a fixture.
const REACHES_GITHUB: &str = "reaches api.github.com";

/// Every endpoint `space()` binds: the exec seam, the listing, the git facades,
/// the GitHub facades. Each declares exactly one action (Source; Meta is the
/// kernel's).
const ENDPOINTS: usize = 2 + GIT_FACADES.len() + GH_FACADES.len();

/// A hermetic scratch repository under a fresh base directory, removed on drop.
struct Scratch {
    /// The ROOT `urn:repo:list` scans: holds exactly `repo`.
    base: PathBuf,
    /// A `git init` with one commit, the `dir=` every git-backed action reads.
    repo: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "ikigai-repo-conformance-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let repo = base.join("scratch");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", repo.to_str().unwrap()])
                // A hermetic identity: no dependency on the machine's config.
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q"]);
        git(&["commit", "-q", "--allow-empty", "-m", "conformance"]);
        Scratch { base, repo }
    }

    fn dir(&self) -> &str {
        self.repo.to_str().unwrap()
    }

    fn root(&self) -> &str {
        self.base.to_str().unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.base).ok();
    }
}

fn kernel() -> Kernel {
    Kernel::new(Arc::new(ikigai_repo::space()))
}

/// The suite, configured for this module (see the file docs for why each line):
/// every fired action reads the scratch repository, and the GitHub facades are
/// opted out.
fn suite(scratch: &Scratch) -> Suite {
    let mut suite = Suite::new()
        .fixture(
            Fixture::new("system-exec", Verb::Source)
                .arg("tool", "git")
                .arg("args", "rev-parse\nHEAD")
                .arg("dir", scratch.dir()),
        )
        .fixture(Fixture::new("repo-list", Verb::Source).arg("root", scratch.root()));
    for id in GIT_FACADES {
        suite = suite.fixture(Fixture::new(id, Verb::Source).arg("dir", scratch.dir()));
    }
    for (id, _) in GH_FACADES {
        suite = suite.opt_out(id, None, REACHES_GITHUB);
    }
    suite
}

/// The walk saw every endpoint, one Source action each, skipped no check, and
/// opted out exactly the GitHub facades. An eleventh endpoint bound without a
/// line here is held to a weaker standard; a listed id that binds nothing is a
/// stale list.
fn assert_shape(report: &Report) {
    assert_eq!(report.endpoints, ENDPOINTS, "{report}");
    assert_eq!(
        report.actions, ENDPOINTS,
        "one Source action each: {report}"
    );
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
    assert_eq!(
        report.declared.opted_out.len(),
        GH_FACADES.len(),
        "only the GitHub facades are opted out: {report}"
    );
}

#[test]
fn conforms() {
    let scratch = Scratch::new();
    let report = suite(&scratch).run_blocking(&kernel());
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);
}

fn request(iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn issue(
    kernel: &Kernel,
    iri: &str,
    args: &[(&str, &str)],
    capability: &Capability,
) -> Result<Representation, Error> {
    futures::executor::block_on(kernel.issue(request(iri, args), capability))
}

/// The ENFORCED half the opt-out blinds: under a capability holding no grants,
/// every GitHub facade refuses with a typed, permanent `Denied` — before `gh` is
/// spawned, so this reaches nothing. `pr=1` is the minimal input the suite
/// would have formed; the listing needs none.
#[test]
fn github_facades_are_denied_under_no_grants() {
    let kernel = kernel();
    let none = Capability::scoped(Vec::<String>::new());
    for (id, iri) in GH_FACADES {
        let err = issue(&kernel, iri, &[("pr", "1")], &none)
            .err()
            .unwrap_or_else(|| panic!("{id} resolved under no grants"));
        assert!(matches!(err, Error::Denied(_)), "{id}: {err:?}");
        assert!(!err.is_transient(), "{id}: {err:?}");
    }
}

/// What `ikigai-conformance` 0.1.0 does not check (its PENDING #11): a declared
/// output that is not an RDF face is never compared with what the action
/// serves. Read by hand, then pinned, for every action the suite fires: the
/// media type served under each `as=` (or none) is one the description
/// declares, and every declared output is served by some call.
#[test]
fn declared_outputs_are_the_media_types_served() {
    let scratch = Scratch::new();
    let kernel = kernel();
    let root = Capability::root();
    let dir = scratch.dir();
    let calls: [(&str, Vec<(&str, &str)>); 6] = [
        (
            "urn:system:exec",
            vec![("tool", "git"), ("args", "rev-parse\nHEAD"), ("dir", dir)],
        ),
        ("urn:repo:status", vec![("dir", dir)]),
        ("urn:repo:branch", vec![("dir", dir)]),
        ("urn:repo:list", vec![("root", scratch.root())]),
        ("urn:repo:log", vec![("dir", dir)]),
        (
            "urn:repo:log",
            vec![("dir", dir), ("as", "application/json")],
        ),
    ];
    let mut served_by_iri: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for (iri, args) in &calls {
        let repr =
            issue(&kernel, iri, args, &root).unwrap_or_else(|e| panic!("{iri} {args:?}: {e}"));
        let got = ikigai_conformance::rdf::bare_media_type(&repr.repr_type.media_type);
        let declared: Vec<String> = kernel
            .describe_pattern(iri)
            .unwrap_or_else(|| panic!("{iri} describes itself"))
            .outputs
            .iter()
            .map(|o| ikigai_conformance::rdf::bare_media_type(o))
            .collect();
        assert!(
            declared.contains(&got),
            "{iri} {args:?} served `{got}`, declared only {declared:?}"
        );
        served_by_iri.entry(iri).or_default().push(got);
    }
    for (iri, served) in served_by_iri {
        let declared: Vec<String> = kernel
            .describe_pattern(iri)
            .unwrap()
            .outputs
            .iter()
            .map(|o| ikigai_conformance::rdf::bare_media_type(o))
            .collect();
        for face in &declared {
            assert!(
                served.contains(face),
                "{iri} declares `{face}` but no call above served it"
            );
        }
    }
}
