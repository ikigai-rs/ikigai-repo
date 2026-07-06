//! Git, `gh`, and `cargo` as ikigai resources.
//!
//! This module is the **platform seam** for development tooling: a
//! capability-gated [`exec`](self) endpoint that runs an *allowlisted* external
//! tool with an **argument vector** (never a shell string — nothing is ever
//! interpolated into a shell, so there is no injection surface), and a set of
//! **typed facades** over it (`urn:repo:status`, `:log`, `:branch`) that build
//! the right invocation and speak the ikigai self-description.
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

use std::process::Command;

use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, ReprType,
    Representation, Result, Verb,
};

/// The tools `urn:system:exec` will spawn. Anything else is refused before a
/// process is created — the allowlist is the outer bound, the capability the
/// inner one.
const ALLOWED_TOOLS: &[&str] = &["git", "gh", "cargo", "just"];

/// Run an allowlisted tool with an argument vector in `dir`, capability-gated.
/// Returns its stdout on success (exit 0); a non-zero exit or a spawn failure is
/// an [`Error::Endpoint`] carrying stderr — so a caller sees *why*, as data.
fn run(inv: &Invocation<'_>, tool: &str, args: &[String], dir: Option<&str>) -> Result<String> {
    if !ALLOWED_TOOLS.contains(&tool) {
        return Err(Error::Endpoint(format!(
            "exec: `{tool}` is not an allowlisted tool ({})",
            ALLOWED_TOOLS.join(", ")
        )));
    }
    let scope = format!("urn:cap:exec:{tool}");
    if !inv.capability.allows(&scope) {
        return Err(Error::Endpoint(format!(
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

        // No exec grant at all → denied.
        let none = Capability::scoped(["urn:cap:unrelated"]);
        let err = source(
            "urn:system:exec",
            &[("tool", "git"), ("args", "--version")],
            &none,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("does not grant"), "{err:?}");

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
}
