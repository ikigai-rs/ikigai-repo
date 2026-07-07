//! Git, `gh`, and `cargo` as ikigai resources.
//!
//! This module is the **platform seam** for development tooling: a
//! capability-gated [`exec`](self) endpoint that runs an *allowlisted* external
//! tool with an **argument vector** (never a shell string — nothing is ever
//! interpolated into a shell, so there is no injection surface), and a set of
//! **typed facades** over it (`urn:repo:status`, `:log`, `:branch`) that build
//! the right invocation and speak the ikigai self-description. A companion
//! `urn:repo:list` enumerates the repositories under a ROOT so an agent whose
//! cwd is not a repo (e.g. `ikigai mcp`) can discover where to point `dir=`.
//!
//! Native-only by nature (it spawns subprocesses), so it carries no wasm face —
//! it is to the shell what `ikigai-personal` is to EventKit.
//!
//! ## The verb tells the truth
//! A read (`git status`, `gh pr view`) is a `Source`; a mutation (`git commit`)
//! would be a `Sink` under a write capability. v1 ships the read facades; the
//! low-level `urn:system:exec` runs whatever allowlisted read the caller builds.
//!
//! ## Capabilities
//! `urn:system:exec` requires `urn:cap:exec:{tool}` (e.g. `urn:cap:exec:git`);
//! the manifold offers it under the wildcard `urn:cap:exec:*`. Each facade
//! declares the concrete capability it needs. No grant, no tool.
//!
//! `urn:repo:list` is a filesystem read, not an exec — it enumerates directory
//! names, never spawning a process — so it declares and enforces
//! `urn:cap:fs:read:*` instead of an `exec:*` scope.

use std::path::PathBuf;
use std::process::Command;

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, ReprType,
    Representation, Result, Verb,
};

/// The tools `urn:system:exec` will spawn. Anything else is refused before a
/// process is created — the allowlist is the outer bound, the capability the
/// inner one.
const ALLOWED_TOOLS: &[&str] = &["git", "gh", "cargo", "just"];

/// Listing the repositories under a ROOT reads directory names — a filesystem
/// read. `urn:repo:list` declares and enforces this scope (the MCP `claude`
/// grant already holds `urn:cap:fs:read:*`). Matching is exact today, so the
/// wildcard token is the literal grant, not a prefix rule.
const FS_READ: &str = "urn:cap:fs:read:*";

/// Run an allowlisted tool with an argument vector in `dir`, capability-gated.
/// Returns its stdout on success (exit 0); a missing capability is a typed,
/// permanent [`Error::Denied`], while a non-zero exit or a spawn failure is an
/// [`Error::Endpoint`] carrying stderr — so a caller sees *why*, as data.
fn run(inv: &Invocation<'_>, tool: &str, args: &[String], dir: Option<&str>) -> Result<String> {
    if !ALLOWED_TOOLS.contains(&tool) {
        return Err(Error::Endpoint(format!(
            "exec: `{tool}` is not an allowlisted tool ({})",
            ALLOWED_TOOLS.join(", ")
        )));
    }
    let scope = format!("urn:cap:exec:{tool}");
    if !inv.capability.allows(&scope) {
        // Typed `Denied` — a permanent authority failure the trace, manifold,
        // and wire recognize as a 403-equivalent without sniffing message text.
        return Err(Error::Denied(format!(
            "exec: capability does not grant `{scope}`"
        )));
    }
    let mut command = Command::new(tool);
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let output = command
        .output()
        .map_err(|e| Error::Endpoint(format!("exec: could not run `{tool}`: {e}")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Endpoint(format!(
            "exec: `{tool}` exited {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )))
    }
}

fn text(body: String) -> Representation {
    Representation::new(
        ReprType::new("text/plain").with_param("charset", "utf-8"),
        body.into_bytes(),
    )
}

/// `urn:system:exec` — the low-level seam. `tool=` (allowlisted) + `args=`
/// (newline-separated argument vector) + optional `dir=`.
fn exec() -> FnEndpoint {
    FnEndpoint::new("system-exec", |inv: &Invocation<'_>| {
        let tool = inv
            .inline_str("tool")
            .map_err(|_| Error::MissingArgument("tool (git, gh, cargo, just)".to_string()))?;
        let args: Vec<String> = inv
            .inline_str("args")
            .unwrap_or("")
            .split('\n')
            .filter(|a| !a.is_empty())
            .map(str::to_string)
            .collect();
        let dir = inv.inline_str("dir").ok();
        run(inv, tool, &args, dir).map(text)
    })
    .with_description(
        Description::new("system-exec")
            .title("Run an allowlisted dev tool")
            .summary(
                "Spawn an allowlisted external tool (git, gh, cargo, just) with an argument \
                 vector — never a shell string, so no injection surface. Capability-gated per \
                 tool (urn:cap:exec:{tool}). The typed urn:repo:* facades build the invocation.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:*")
            .input(
                ArgSpec::new("tool")
                    .summary("the tool to run")
                    .one_of(ALLOWED_TOOLS.iter().copied()),
            )
            .input(
                ArgSpec::new("args")
                    .summary("the argument vector, one argument per line")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .summary("working directory (defaults to the process cwd)")
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// A `git`-backed read facade: builds `git -C <dir> <args…>` and runs it.
fn git_facade(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    git_args: &'static [&'static str],
) -> FnEndpoint {
    FnEndpoint::new(id, move |inv: &Invocation<'_>| {
        let mut args: Vec<String> = Vec::new();
        if let Ok(dir) = inv.inline_str("dir") {
            args.push("-C".to_string());
            args.push(dir.to_string());
        }
        args.extend(git_args.iter().map(|a| a.to_string()));
        run(inv, "git", &args, None).map(text)
    })
    .with_description(
        Description::new(id)
            .title(title)
            .summary(summary)
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:git")
            .input(
                ArgSpec::new("dir")
                    .summary("the repository directory (defaults to the process cwd)")
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// A `gh`-backed read facade over a pull request: `gh <sub…> <pr> [--repo R]`,
/// run in `dir` (or against `--repo owner/name`). `pr=` is required.
fn gh_pr_facade(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    sub: &'static [&'static str],
) -> FnEndpoint {
    FnEndpoint::new(id, move |inv: &Invocation<'_>| {
        let pr = inv
            .inline_str("pr")
            .map_err(|_| Error::MissingArgument("pr (the pull-request number)".to_string()))?;
        let mut args: Vec<String> = sub.iter().map(|a| a.to_string()).collect();
        args.push(pr.to_string());
        if let Ok(repo) = inv.inline_str("repo") {
            args.push("--repo".to_string());
            args.push(repo.to_string());
        }
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir).map(text)
    })
    .with_description(
        Description::new(id)
            .title(title)
            .summary(summary)
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:gh")
            .input(
                ArgSpec::new("pr")
                    .class("http://www.w3.org/2001/XMLSchema#integer")
                    .summary("the pull-request number"),
            )
            .input(
                ArgSpec::new("repo")
                    .summary("owner/name (else the repo at dir=/cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .summary("a repo directory to run in (else the process cwd)")
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// Where `urn:repo:list` scans, in priority order: an explicit `root=` arg,
/// then the `IKIGAI_REPO_ROOT` env var (the IKIGAI_* config convention), then
/// `~/git-personal` — this ecosystem's home for its sibling repos.
fn repo_root(inv: &Invocation<'_>) -> Result<PathBuf> {
    if let Ok(root) = inv.inline_str("root") {
        return Ok(PathBuf::from(root));
    }
    if let Ok(root) = std::env::var("IKIGAI_REPO_ROOT") {
        return Ok(PathBuf::from(root));
    }
    let home = std::env::var("HOME").map_err(|_| {
        Error::Endpoint("repo-list: no root=, no IKIGAI_REPO_ROOT, and $HOME is unset".to_string())
    })?;
    Ok(PathBuf::from(home).join("git-personal"))
}

/// `urn:repo:list` — enumerate the git repositories under a ROOT, one
/// `name<TAB>path` per line, so an agent whose cwd is *not* a repo (the `ikigai
/// mcp` case) can discover where repos live and hand one back as `dir=` to the
/// `urn:repo:*` facades.
///
/// A repo is an immediate child directory of ROOT that holds a `.git` entry.
/// One level deep is enough for this flat ecosystem (the repos are siblings
/// under `~/git-personal`); we deliberately do NOT recurse. Listing directory
/// names is a filesystem read — this crate is native-only by design, so it
/// reads the directory with `std::fs` directly rather than through a kernel fs
/// mount — gated on [`FS_READ`], never shelling out (so no exec cap).
fn list() -> FnEndpoint {
    FnEndpoint::new("repo-list", |inv: &Invocation<'_>| {
        if !inv.capability.allows(FS_READ) {
            // Typed `Denied` — a permanent authority failure the trace, manifold,
            // and wire recognize as a 403-equivalent without sniffing message text.
            return Err(Error::Denied(format!(
                "repo-list: capability does not grant `{FS_READ}`"
            )));
        }
        let root = repo_root(inv)?;
        let entries = std::fs::read_dir(&root).map_err(|e| {
            Error::Endpoint(format!(
                "repo-list: cannot read root `{}`: {e}",
                root.display()
            ))
        })?;
        let mut repos: Vec<(String, String)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            // One level deep only: an immediate child dir carrying a `.git`.
            if path.is_dir() && path.join(".git").exists() {
                // Skip non-UTF-8 names rather than mangle them.
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                // Absolute path; fall back to the joined path if canonicalize fails.
                let abs = path.canonicalize().unwrap_or(path);
                repos.push((name, abs.display().to_string()));
            }
        }
        repos.sort();
        let body = repos
            .into_iter()
            .map(|(name, path)| format!("{name}\t{path}"))
            .collect::<Vec<_>>()
            .join("\n");
        // TODO(turtle): an `as=text/turtle` face — skolemized `urn:repo:{name}`
        // with rdfs:label + a path predicate — is a clean follow-up, but the
        // path predicate is a vocab decision (ikigai-rs.dev/ns#) that belongs
        // upstream; left for the hub rather than minting a term here.
        Ok(text(body))
    })
    .with_description(
        Description::new("repo-list")
            .title("List repositories")
            .summary(
                "Enumerate the git repositories under a ROOT — each as name<TAB>path, one per \
                 line — so an agent whose cwd is not a repo (e.g. ikigai mcp) can discover where \
                 repos live and pass one as dir= to the urn:repo:* facades. ROOT is root= (arg), \
                 else $IKIGAI_REPO_ROOT, else ~/git-personal; immediate child directories only.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(FS_READ)
            .input(
                ArgSpec::new("root")
                    .summary(
                        "the directory to scan (default: $IKIGAI_REPO_ROOT, else ~/git-personal)",
                    )
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// The dev-tooling space: the exec seam + the read facades.
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:system:exec"), exec())
        .bind(
            Exact::new("urn:repo:status"),
            git_facade(
                "repo-status",
                "Repository status",
                "The working tree's status, machine-readable (git status --porcelain=v1 -b).",
                &["status", "--porcelain=v1", "-b"],
            ),
        )
        .bind(
            Exact::new("urn:repo:log"),
            git_facade(
                "repo-log",
                "Recent history",
                "The last 20 commits, one line each (git log --oneline -20).",
                &["log", "--oneline", "-20"],
            ),
        )
        .bind(
            Exact::new("urn:repo:branch"),
            git_facade(
                "repo-branch",
                "Current branch",
                "The current branch name (git branch --show-current).",
                &["branch", "--show-current"],
            ),
        )
        .bind(Exact::new("urn:repo:list"), list())
        .bind(
            Exact::new("urn:repo:pr:checks"),
            gh_pr_facade(
                "repo-pr-checks",
                "PR check status",
                "The CI check runs for a pull request and their state (gh pr checks). A \
                 SNAPSHOT — for a blocking wait use gh's own --watch; the standing poll job \
                 (host-side, time transport) is what removes the wait from an agent's loop.",
                &["pr", "checks"],
            ),
        )
        .bind(
            Exact::new("urn:repo:pr:view"),
            gh_pr_facade(
                "repo-pr-view",
                "PR overview",
                "A pull request's title, state, and metadata (gh pr view).",
                &["pr", "view"],
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request};
    use std::sync::Arc;

    fn kernel() -> Kernel {
        Kernel::new(Arc::new(space()))
    }

    fn source(iri: &str, args: &[(&str, &str)], cap: &Capability) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        block_on(kernel().issue(request, cap))
    }

    #[test]
    fn exec_is_capability_gated_and_allowlisted() {
        let git = Capability::scoped(["urn:cap:exec:git"]);

        // No exec grant at all → denied. A capability denial is the typed,
        // permanent `Denied` — never a generic `Endpoint` string, and never
        // transient (re-issuing under the same capability won't change the answer).
        let none = Capability::scoped(["urn:cap:unrelated"]);
        let err = source(
            "urn:system:exec",
            &[("tool", "git"), ("args", "--version")],
            &none,
        )
        .unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");

        // A non-allowlisted tool → refused before any process, even under a matching cap.
        let evil = Capability::scoped(["urn:cap:exec:rm"]);
        let err = source("urn:system:exec", &[("tool", "rm"), ("args", "-rf")], &evil).unwrap_err();
        assert!(
            format!("{err:?}").contains("not an allowlisted tool"),
            "{err:?}"
        );

        // git --version under the git grant → runs, stdout carries "git version".
        let out = source(
            "urn:system:exec",
            &[("tool", "git"), ("args", "--version")],
            &git,
        )
        .unwrap();
        assert!(
            String::from_utf8_lossy(&out.bytes).contains("git version"),
            "{:?}",
            String::from_utf8_lossy(&out.bytes)
        );
    }

    #[test]
    fn gh_facade_is_gated_and_requires_pr() {
        // No exec:gh grant → denied (typed, permanent) before any gh runs.
        let bare = Capability::scoped(["urn:cap:exec:git"]);
        let err = source("urn:repo:pr:checks", &[("pr", "1")], &bare).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");

        // With the grant but no pr= → a clean missing-argument error (still no gh run).
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        let err = source("urn:repo:pr:checks", &[], &gh).unwrap_err();
        assert!(format!("{err:?}").contains("MissingArgument"), "{err:?}");
    }

    #[test]
    fn a_facade_declares_its_capability_and_reads_this_repo() {
        // The facade requires exec:git; without it, denied.
        let bare = Capability::scoped(["urn:cap:unrelated"]);
        assert!(source("urn:repo:branch", &[], &bare).is_err());

        // With it, `urn:repo:branch` reads the current repo (this crate's dir).
        let git = Capability::scoped(["urn:cap:exec:git"]);
        let out = source("urn:repo:branch", &[], &git);
        // In CI/checkout this resolves to a branch name (or empty on detached HEAD);
        // either way it must not error under the right capability.
        assert!(out.is_ok(), "{out:?}");
    }

    #[test]
    fn list_enumerates_repos_under_root() {
        // A self-contained ROOT: two fake repos (dirs holding a `.git`) plus a
        // decoy non-repo dir — no dependency on ~/git-personal.
        let base =
            std::env::temp_dir().join(format!("ikigai-repo-list-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(base.join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(base.join("not-a-repo")).unwrap();

        let fs = Capability::scoped(["urn:cap:fs:read:*"]);
        let out = source("urn:repo:list", &[("root", base.to_str().unwrap())], &fs).unwrap();
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        let lines: Vec<&str> = body.lines().collect();

        // Only the two `.git`-bearing dirs, sorted, name<TAB>path; decoy excluded.
        assert_eq!(lines.len(), 2, "{body:?}");
        assert!(lines[0].starts_with("alpha\t"), "{body:?}");
        assert!(lines[1].starts_with("beta\t"), "{body:?}");
        // The path column is the absolute repo directory.
        let alpha_path = lines[0].split('\t').nth(1).unwrap();
        assert!(alpha_path.ends_with("alpha"), "{body:?}");
        assert!(std::path::Path::new(alpha_path).is_absolute(), "{body:?}");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn list_is_capability_gated() {
        // No fs:read grant → denied before any directory is read. The typed,
        // permanent `Denied` (a 403-equivalent), never transient.
        let none = Capability::scoped(["urn:cap:unrelated"]);
        let err = source("urn:repo:list", &[("root", "/tmp")], &none).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");
    }
}
