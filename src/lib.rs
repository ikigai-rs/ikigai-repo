//! Git, `gh`, and `cargo` as ikigai resources.
//!
//! This module is the **platform seam** for development tooling: a
//! capability-gated [`exec`](self) endpoint that runs an *allowlisted* external
//! tool with an **argument vector** (never a shell string — nothing is ever
//! interpolated into a shell, so there is no injection surface), and a set of
//! **typed facades** over it (`urn:repo:status`, `:log`, `:branch`, and the
//! `urn:repo:pr:*` family: `:list`, `:view`, `:files`, `:diff`, `:checks`) that build
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

/// The XSD datatype every scalar input here carries. Paths (`dir`, `root`,
/// `path`), an `owner/name` slug, a face selector, a state word, a tool name:
/// all strings on the wire. `args` is the one list-valued input — a
/// newline-separated argument vector — and there is no ArgSpec spelling for a
/// list, so it is declared as the string the wire carries.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";

/// The XSD datatype of the counted inputs (`pr`, `limit`).
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

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

fn json(body: String) -> Representation {
    Representation::new(ReprType::new("application/json"), body.into_bytes())
}

/// The `as=` face convention (see ikigai-org / ikigai-personal): a face is
/// selected by substring, so `as=application/json` and `as=json` both work.
fn wants_json(inv: &Invocation<'_>) -> bool {
    inv.inline_str("as")
        .map(|s| s.contains("json"))
        .unwrap_or(false)
}

/// Parse a `limit=` argument as a count. Arguments are passed as an argv (no
/// shell), so this is not an injection guard — it stops a stray value from
/// becoming a *flag* (`-{limit}` with a leading dash) and gives a clean error.
fn parse_limit(inv: &Invocation<'_>, default: u32) -> Result<u32> {
    match inv.inline_str("limit") {
        Ok(s) => s
            .parse()
            .map_err(|_| Error::Endpoint(format!("limit must be a number, got `{s}`"))),
        Err(_) => Ok(default),
    }
}

/// Parse the required `pr=` argument as a PR number. Like [`parse_limit`],
/// this is a flag guard, not a shell guard (there is no shell): the value is
/// positional in the gh argv, so a stray `--web` or `--repo …` would otherwise
/// become a *flag* to gh. Digits-only also matches the declared xsd:integer
/// class — the description already promises a number.
fn parse_pr(inv: &Invocation<'_>) -> Result<u64> {
    let s = inv
        .inline_str("pr")
        .map_err(|_| Error::MissingArgument("pr (the pull-request number)".to_string()))?;
    s.parse()
        .map_err(|_| Error::Endpoint(format!("pr must be a number, got `{s}`")))
}

/// Parse an optional `path=` argument as a pathspec. The value travels in the
/// argv *after* `--` — git's own "everything past here is a path" delimiter —
/// so it can never be read as a flag or a rev; the guard, like [`parse_limit`],
/// exists to refuse a value that *looks* like a flag with a clean error
/// instead of quietly matching nothing, and to stop the empty string (git
/// rejects the empty pathspec with its own fatal).
fn parse_path(inv: &Invocation<'_>) -> Result<Option<String>> {
    match inv.inline_str("path") {
        Ok("") => Err(Error::Endpoint("path must not be empty".to_string())),
        Ok(s) if s.starts_with('-') => Err(Error::Endpoint(format!(
            "path must be a pathspec, not a flag: got `{s}`"
        ))),
        Ok(s) => Ok(Some(s.to_string())),
        Err(_) => Ok(None),
    }
}

/// Escape a string as a JSON string literal (quotes included). The JSON faces
/// this crate emits are built by hand — the shapes are flat and small, and a
/// serde_json dependency for four fields would be all cost.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
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
                    .class(XSD_STRING)
                    .summary("the tool to run")
                    .one_of(ALLOWED_TOOLS.iter().copied()),
            )
            .input(
                ArgSpec::new("args")
                    .class(XSD_STRING)
                    .summary("the argument vector, one argument per line")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .class(XSD_STRING)
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
                    .class(XSD_STRING)
                    .summary("the repository directory (defaults to the process cwd)")
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// A `gh`-backed read facade over a pull request: `gh <sub…> <pr> [--repo R]`,
/// run in `dir` (or against `--repo owner/name`). `pr=` is required. When
/// `json_fields` is given, `as=application/json` selects a structured face
/// (`gh … --json <fields>`), passed through verbatim as `application/json`.
fn gh_pr_facade(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    sub: &'static [&'static str],
    json_fields: Option<&'static str>,
) -> FnEndpoint {
    FnEndpoint::new(id, move |inv: &Invocation<'_>| {
        let pr = parse_pr(inv)?;
        let mut args: Vec<String> = sub.iter().map(|a| a.to_string()).collect();
        args.push(pr.to_string());
        let want_json = json_fields.is_some() && wants_json(inv);
        if let (Some(fields), true) = (json_fields, want_json) {
            args.push("--json".to_string());
            args.push(fields.to_string());
        }
        if let Ok(repo) = inv.inline_str("repo") {
            args.push("--repo".to_string());
            args.push(repo.to_string());
        }
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir).map(if want_json { json } else { text })
    })
    .with_description({
        let mut description = Description::new(id)
            .title(title)
            .summary(summary)
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:gh")
            .input(
                ArgSpec::new("pr")
                    .class(XSD_INTEGER)
                    .summary("the pull-request number"),
            )
            .input(
                ArgSpec::new("repo")
                    .class(XSD_STRING)
                    .summary("owner/name (else the repo at dir=/cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .class(XSD_STRING)
                    .summary("a repo directory to run in (else the process cwd)")
                    .optional(),
            );
        if let Some(fields) = json_fields {
            description = description
                .input(
                    ArgSpec::new("as")
                        .class(XSD_STRING)
                        .summary(format!(
                            "application/json for the structured face ({fields})"
                        ))
                        .one_of(["application/json"])
                        .optional(),
                )
                .output("text/plain;charset=utf-8")
                .output("application/json");
        } else {
            description = description.output("text/plain;charset=utf-8");
        }
        description
    })
}

/// The fields the structured `urn:repo:pr:view` face carries. `headRefOid` is
/// the PR head commit sha — the key a downstream review archive stores under
/// (a review is *of* a commit, not of a mutable branch tip).
const PR_VIEW_JSON_FIELDS: &str =
    "number,title,state,isDraft,headRefName,headRefOid,baseRefName,author,updatedAt,url,body";

/// The fields both faces of `urn:repo:pr:list` are built from. `state` is
/// gh's own value — `OPEN` / `CLOSED` / `MERGED`, uppercase — so a mixed
/// (`state=all`) listing stays legible.
const PR_LIST_FIELDS: &str = "number,title,state,headRefName,updatedAt";

/// The states `urn:repo:pr:list` will filter by — `gh pr list --state`'s own
/// vocabulary, `open` being gh's (and our) default.
const PR_LIST_STATES: &[&str] = &["open", "closed", "merged", "all"];

/// The gh Go-template for the text face of `urn:repo:pr:list`: one PR per
/// line, `number⇥title⇥state⇥branch⇥updated` (RFC 3339). Real tab/newline
/// characters — arguments travel as an argv, never through a shell.
const PR_LIST_TEMPLATE: &str = concat!(
    "{{range .}}{{.number}}\t{{.title}}\t{{.state}}\t{{.headRefName}}\t",
    "{{timefmt \"2006-01-02T15:04:05Z07:00\" .updatedAt}}\n{{end}}"
);

/// Build the `gh pr list` argv. `gh pr list`'s own order is newest-CREATED
/// first; a listing exists to answer "what moved lately", so we always ask for
/// most-recently-UPDATED first. The search qualifier is the only sort gh
/// exposes: `--search "sort:updated-desc"` routes the listing through the
/// search API, which composes with `--state` (gh folds it into the query;
/// `merged` becomes `is:merged`) and leaves both faces' shapes untouched
/// (verified against repos where creation and update order diverge).
fn pr_list_args(state: &str, limit: u32, want_json: bool, repo: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["pr", "list", "--state"]
        .iter()
        .map(|a| a.to_string())
        .collect();
    args.push(state.to_string());
    args.push("--limit".to_string());
    args.push(limit.to_string());
    args.push("--search".to_string());
    args.push("sort:updated-desc".to_string());
    args.push("--json".to_string());
    args.push(PR_LIST_FIELDS.to_string());
    if !want_json {
        args.push("--template".to_string());
        args.push(PR_LIST_TEMPLATE.to_string());
    }
    if let Some(repo) = repo {
        args.push("--repo".to_string());
        args.push(repo.to_string());
    }
    args
}

/// The gh Go-template for the text face of `urn:repo:pr:files`: one changed
/// path per line. A real newline — arguments travel as an argv, never through
/// a shell.
const PR_FILES_TEMPLATE: &str = "{{range .files}}{{.path}}\n{{end}}";

/// Build the `gh pr view --json files` argv both faces of `urn:repo:pr:files`
/// share. Like `pr_list_args`, the text face renders the same `--json` export
/// through a template (one path per line) so the faces cannot drift; the json
/// face passes gh's export through verbatim.
fn pr_files_args(pr: u64, want_json: bool, repo: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["pr", "view"].iter().map(|a| a.to_string()).collect();
    args.push(pr.to_string());
    args.push("--json".to_string());
    args.push("files".to_string());
    if !want_json {
        args.push("--template".to_string());
        args.push(PR_FILES_TEMPLATE.to_string());
    }
    if let Some(repo) = repo {
        args.push("--repo".to_string());
        args.push(repo.to_string());
    }
    args
}

/// `urn:repo:pr:files` — the paths a pull request changes. The open-PR half of
/// "which PRs touch this subtree" (`urn:repo:log path=` is the merged half):
/// intersect these paths with the subtree you are looking at. Default face is
/// one path per line; `as=application/json` passes gh's export through —
/// `{"files": [{path, additions, deletions, changeType}]}`.
fn pr_files() -> FnEndpoint {
    FnEndpoint::new("repo-pr-files", |inv: &Invocation<'_>| {
        let pr = parse_pr(inv)?;
        let want_json = wants_json(inv);
        let repo = inv.inline_str("repo").ok();
        let args = pr_files_args(pr, want_json, repo);
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir).map(if want_json { json } else { text })
    })
    .with_description(
        Description::new("repo-pr-files")
            .title("PR changed files")
            .summary(
                "The paths a pull request changes (gh pr view --json files), one per line; \
                 as=application/json for the structured face ({files: [{path, additions, \
                 deletions, changeType}]}). The open-PR half of \"which PRs touch this \
                 subtree\" — urn:repo:log path= is the merged half.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:gh")
            .input(
                ArgSpec::new("pr")
                    .class(XSD_INTEGER)
                    .summary("the pull-request number"),
            )
            .input(
                ArgSpec::new("repo")
                    .class(XSD_STRING)
                    .summary("owner/name (else the repo at dir=/cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .class(XSD_STRING)
                    .summary("a repo directory to run in (else the process cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("as")
                    .class(XSD_STRING)
                    .summary("application/json for the structured face")
                    .one_of(["application/json"])
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
            .output("application/json"),
    )
}

/// `urn:repo:pr:list` — the pull requests, most recently updated first,
/// machine-readable. `state=` filters (open by default; `all` for a mixed
/// listing). Both faces are built from the same `--json` export so they cannot
/// drift: the default text face renders it through a gh template (one PR per
/// line, `number⇥title⇥state⇥branch⇥updated`), `as=application/json` passes it
/// through.
fn pr_list() -> FnEndpoint {
    FnEndpoint::new("repo-pr-list", |inv: &Invocation<'_>| {
        let state = inv.inline_str("state").unwrap_or("open");
        if !PR_LIST_STATES.contains(&state) {
            // Refused before gh is spawned — a stray value must never become
            // part of the search invocation.
            return Err(Error::Endpoint(format!(
                "state must be one of {}, got `{state}`",
                PR_LIST_STATES.join(", ")
            )));
        }
        let limit = parse_limit(inv, 30)?;
        let want_json = wants_json(inv);
        let repo = inv.inline_str("repo").ok();
        let args = pr_list_args(state, limit, want_json, repo);
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir).map(if want_json { json } else { text })
    })
    .with_description(
        Description::new("repo-pr-list")
            .title("Pull requests")
            .summary(
                "The pull requests, most recently updated first (gh pr list), one per line as \
                 number<TAB>title<TAB>state<TAB>branch<TAB>updated (RFC 3339, state \
                 OPEN/CLOSED/MERGED); state= filters (default open, all for a mixed listing); \
                 as=application/json for the structured face (number, title, state, headRefName, \
                 updatedAt). Pass a number to urn:repo:pr:view / :diff / :checks.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:gh")
            .input(
                ArgSpec::new("state")
                    .class(XSD_STRING)
                    .summary("which PRs to list by state")
                    .one_of(PR_LIST_STATES.iter().copied())
                    .default_value("open"),
            )
            .input(
                ArgSpec::new("limit")
                    .class(XSD_INTEGER)
                    .summary("the maximum number of PRs to list")
                    .default_value("30"),
            )
            .input(
                ArgSpec::new("repo")
                    .class(XSD_STRING)
                    .summary("owner/name (else the repo at dir=/cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir")
                    .class(XSD_STRING)
                    .summary("a repo directory to run in (else the process cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("as")
                    .class(XSD_STRING)
                    .summary("application/json for the structured face")
                    .one_of(["application/json"])
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
            .output("application/json"),
    )
}

/// `urn:repo:log` — recent history. The default face is `git log --oneline`;
/// `as=application/json` renders `[{hash, author, date, subject}]` (full sha,
/// author name, RFC 3339 author date) parsed from a unit-separator-delimited
/// pretty format — `%x1f` cannot appear in a commit subject that any normal
/// tool produced, and a subject that somehow carries one merely truncates its
/// own entry's subject field.
///
/// `path=` restricts either face to the commits that touched a file or
/// subtree (`git log … -- <path>`), composing with `limit=`. Under the house
/// squash-merge style — `(#N)` at the end of every merge subject — a
/// path-scoped log IS the merged-PR-per-path index; extracting the numbers
/// stays downstream (a presentation concern), this face delivers honest
/// subjects.
fn log() -> FnEndpoint {
    FnEndpoint::new("repo-log", |inv: &Invocation<'_>| {
        let limit = parse_limit(inv, 20)?;
        let path = parse_path(inv)?;
        let want_json = wants_json(inv);
        let mut args: Vec<String> = Vec::new();
        if let Ok(dir) = inv.inline_str("dir") {
            args.push("-C".to_string());
            args.push(dir.to_string());
        }
        args.push("log".to_string());
        if want_json {
            args.push("--pretty=format:%H%x1f%an%x1f%aI%x1f%s".to_string());
        } else {
            args.push("--oneline".to_string());
        }
        args.push(format!("-{limit}"));
        // The pathspec rides after `--`, so it reaches git as a path and only
        // a path — the same argv either face builds, shape untouched.
        if let Some(path) = path {
            args.push("--".to_string());
            args.push(path);
        }
        let raw = run(inv, "git", &args, None)?;
        if want_json {
            let entries: Vec<String> = raw
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(4, '\x1f');
                    match (parts.next(), parts.next(), parts.next(), parts.next()) {
                        (Some(hash), Some(author), Some(date), Some(subject)) => Some(format!(
                            "{{\"hash\":{},\"author\":{},\"date\":{},\"subject\":{}}}",
                            json_str(hash),
                            json_str(author),
                            json_str(date),
                            json_str(subject)
                        )),
                        _ => None,
                    }
                })
                .collect();
            Ok(json(format!("[{}]", entries.join(","))))
        } else {
            Ok(text(raw))
        }
    })
    .with_description(
        Description::new("repo-log")
            .title("Recent history")
            .summary(
                "The recent commits: one line each by default (git log --oneline); \
                 as=application/json for [{hash, author, date, subject}] with the full sha \
                 and RFC 3339 author date. path= restricts to the commits that touched a \
                 file or subtree (git log -- <path>) — under squash-merge style, the \
                 merged-PR-per-path index.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:git")
            .input(
                ArgSpec::new("limit")
                    .class(XSD_INTEGER)
                    .summary("the number of commits to show")
                    .default_value("20"),
            )
            .input(
                ArgSpec::new("path").class(XSD_STRING)
                    .summary("restrict to commits touching this file or subtree, relative to the repo root")
                    .optional(),
            )
            .input(
                ArgSpec::new("dir").class(XSD_STRING)
                    .summary("the repository directory (defaults to the process cwd)")
                    .optional(),
            )
            .input(
                ArgSpec::new("as").class(XSD_STRING)
                    .summary("application/json for the structured face")
                    .one_of(["application/json"])
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
            .output("application/json"),
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
                    .class(XSD_STRING)
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
        .bind(Exact::new("urn:repo:log"), log())
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
                None,
            ),
        )
        .bind(
            Exact::new("urn:repo:pr:view"),
            gh_pr_facade(
                "repo-pr-view",
                "PR overview",
                "A pull request's title, state, and metadata (gh pr view); \
                 as=application/json for the structured face, which carries headRefOid — \
                 the PR head commit sha a review archive keys on.",
                &["pr", "view"],
                Some(PR_VIEW_JSON_FIELDS),
            ),
        )
        .bind(Exact::new("urn:repo:pr:list"), pr_list())
        .bind(Exact::new("urn:repo:pr:files"), pr_files())
        .bind(
            Exact::new("urn:repo:pr:diff"),
            gh_pr_facade(
                "repo-pr-diff",
                "PR diff",
                "A pull request's unified diff (gh pr diff) — the raw change an explainer \
                 or reviewer reads. Pair with urn:repo:pr:view as=application/json for the \
                 head sha the diff corresponds to.",
                &["pr", "diff"],
                None,
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
    fn pr_list_and_diff_are_gated() {
        // No exec:gh grant → typed, permanent Denied before any gh runs.
        let bare = Capability::scoped(["urn:cap:exec:git"]);
        let err = source("urn:repo:pr:list", &[], &bare).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        let err = source("urn:repo:pr:diff", &[("pr", "1")], &bare).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // pr:diff without pr= → a clean missing-argument error (still no gh run).
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        let err = source("urn:repo:pr:diff", &[], &gh).unwrap_err();
        assert!(format!("{err:?}").contains("MissingArgument"), "{err:?}");

        // A non-numeric limit= is refused before gh is spawned — a stray value
        // must never become a flag (`-{limit}`).
        let err = source("urn:repo:pr:list", &[("limit", "nope")], &gh).unwrap_err();
        assert!(
            format!("{err:?}").contains("limit must be a number"),
            "{err:?}"
        );

        // A state outside the one_of vocabulary is refused before gh is
        // spawned, and the error names the valid values.
        let err = source("urn:repo:pr:list", &[("state", "draft")], &gh).unwrap_err();
        assert!(
            format!("{err:?}").contains("state must be one of open, closed, merged, all"),
            "{err:?}"
        );
    }

    /// The `gh pr list` argv is the contract with gh: state filter, recency
    /// sort, and the fields both faces are built from.
    #[test]
    fn pr_list_builds_the_gh_invocation() {
        // Default shape (text face): state open, always sorted by recency of
        // update via the search API (gh's own order is newest-created first),
        // and the template renders the five-column line — state included, so a
        // mixed listing is legible.
        let args = pr_list_args("open", 30, false, None);
        assert_eq!(args[..4], ["pr", "list", "--state", "open"]);
        let flag = |name: &str| {
            let at = args.iter().position(|a| a == name).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(flag("--limit"), "30");
        assert_eq!(flag("--search"), "sort:updated-desc");
        assert_eq!(flag("--json"), PR_LIST_FIELDS);
        assert!(PR_LIST_FIELDS.contains("state"), "{PR_LIST_FIELDS}");
        let template = flag("--template");
        assert_eq!(
            template.matches('\t').count(),
            4,
            "five columns: {template:?}"
        );
        assert!(template.contains("{{.state}}"), "{template:?}");

        // JSON face: no template — the export passes through verbatim, so the
        // structured face carries exactly PR_LIST_FIELDS. repo= appends.
        let args = pr_list_args("all", 10, true, Some("owner/name"));
        assert!(args.iter().all(|a| a != "--template"), "{args:?}");
        assert_eq!(args[3], "all");
        assert_eq!(args[args.len() - 2..], ["--repo", "owner/name"]);
    }

    /// The describe() shapes ARE the module's contract — the engine routes
    /// named args by them, selection matches on them, MCP projects them.
    #[test]
    fn descriptions_declare_faces_args_and_caps() {
        use ikigai_core::Endpoint;

        // pr:list: exec:gh, limit default 30 (xsd:integer), a json face, both outputs.
        let d = pr_list().describe();
        assert!(d.requires.contains(&"urn:cap:exec:gh".to_string()), "{d:?}");
        let limit = d.inputs.iter().find(|a| a.name == "limit").unwrap();
        assert_eq!(limit.default.as_deref(), Some("30"));
        assert!(!limit.required);
        assert_eq!(
            limit.class.as_deref(),
            Some("http://www.w3.org/2001/XMLSchema#integer")
        );
        let face = d.inputs.iter().find(|a| a.name == "as").unwrap();
        assert_eq!(face.one_of, vec!["application/json".to_string()]);
        assert!(d.outputs.iter().any(|o| o == "application/json"), "{d:?}");
        assert!(
            d.outputs.iter().any(|o| o.starts_with("text/plain")),
            "{d:?}"
        );
        // state: an enum arg — one_of is the vocabulary, default preserves the
        // original open-only behavior.
        let state = d.inputs.iter().find(|a| a.name == "state").unwrap();
        assert_eq!(state.one_of, PR_LIST_STATES.to_vec());
        assert_eq!(state.default.as_deref(), Some("open"));
        assert!(!state.required);

        // pr:view: gains the json face (it carries headRefOid — the head sha).
        let d = gh_pr_facade(
            "repo-pr-view",
            "t",
            "s",
            &["pr", "view"],
            Some(PR_VIEW_JSON_FIELDS),
        )
        .describe();
        let face = d.inputs.iter().find(|a| a.name == "as").unwrap();
        assert!(face.summary.contains("headRefOid"), "{face:?}");
        assert!(d.outputs.iter().any(|o| o == "application/json"), "{d:?}");

        // pr:diff: text-only (no as= arg, one output), pr= required.
        let d = gh_pr_facade("repo-pr-diff", "t", "s", &["pr", "diff"], None).describe();
        assert!(d.inputs.iter().all(|a| a.name != "as"), "{d:?}");
        assert_eq!(d.outputs, vec!["text/plain;charset=utf-8".to_string()]);
        assert!(d.inputs.iter().find(|a| a.name == "pr").unwrap().required);

        // log: exec:git, limit default 20, both faces, path= optional.
        let d = log().describe();
        assert!(
            d.requires.contains(&"urn:cap:exec:git".to_string()),
            "{d:?}"
        );
        let limit = d.inputs.iter().find(|a| a.name == "limit").unwrap();
        assert_eq!(limit.default.as_deref(), Some("20"));
        assert!(d.outputs.iter().any(|o| o == "application/json"), "{d:?}");
        let path = d.inputs.iter().find(|a| a.name == "path").unwrap();
        assert!(!path.required, "{path:?}");

        // pr:files: exec:gh, pr= required (xsd:integer), a json face, both outputs.
        let d = pr_files().describe();
        assert!(d.requires.contains(&"urn:cap:exec:gh".to_string()), "{d:?}");
        let pr = d.inputs.iter().find(|a| a.name == "pr").unwrap();
        assert!(pr.required, "{pr:?}");
        assert_eq!(
            pr.class.as_deref(),
            Some("http://www.w3.org/2001/XMLSchema#integer")
        );
        let face = d.inputs.iter().find(|a| a.name == "as").unwrap();
        assert_eq!(face.one_of, vec!["application/json".to_string()]);
        assert!(d.outputs.iter().any(|o| o == "application/json"), "{d:?}");
        assert!(
            d.outputs.iter().any(|o| o.starts_with("text/plain")),
            "{d:?}"
        );
    }

    #[test]
    fn log_faces_read_a_scratch_repo() {
        // A self-contained repo with exactly three commits — the checkout CI
        // runs in is SHALLOW (fetch-depth 1), so this crate's own history is
        // one merge commit deep there; never assert against it. This also
        // exercises dir= (the facade's `git -C`).
        let base =
            std::env::temp_dir().join(format!("ikigai-repo-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let git_in = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", base.to_str().unwrap()])
                // A hermetic identity: no dependency on the machine's config.
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git_in(&["init", "-q"]);
        for subject in ["first", "second \"quoted\"", "third"] {
            git_in(&["commit", "-q", "--allow-empty", "-m", subject]);
        }
        let dir = base.to_str().unwrap();
        let git = Capability::scoped(["urn:cap:exec:git"]);

        // Default face: oneline, limit honored.
        let out = source("urn:repo:log", &[("limit", "2"), ("dir", dir)], &git).unwrap();
        assert!(out.repr_type.media_type.starts_with("text/plain"));
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 2, "{body:?}");
        assert!(body.lines().next().unwrap().contains("third"), "{body:?}");

        // JSON face: [{hash, author, date, subject}], full sha, RFC 3339 date.
        let out = source(
            "urn:repo:log",
            &[("limit", "2"), ("dir", dir), ("as", "application/json")],
            &git,
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "application/json");
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert!(body.starts_with('[') && body.ends_with(']'), "{body:?}");
        assert_eq!(body.matches("\"hash\":").count(), 2, "{body:?}");
        for key in ["\"author\":", "\"date\":", "\"subject\":"] {
            assert_eq!(body.matches(key).count(), 2, "{body:?}");
        }
        // The first hash is a full 40-hex sha; the date is RFC 3339 (has a 'T').
        let hash = body.split("\"hash\":\"").nth(1).unwrap();
        let hash = &hash[..hash.find('"').unwrap()];
        assert_eq!(hash.len(), 40, "{hash:?}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()), "{hash:?}");
        let date = body.split("\"date\":\"").nth(1).unwrap();
        assert!(date[..date.find('"').unwrap()].contains('T'), "{body:?}");
        // A quote in a commit subject is escaped, not a broken document.
        assert!(body.contains("second \\\"quoted\\\""), "{body:?}");

        // A non-numeric limit is refused before git runs.
        let err = source("urn:repo:log", &[("limit", "1; rm")], &git).unwrap_err();
        assert!(
            format!("{err:?}").contains("limit must be a number"),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn log_path_scopes_a_scratch_repo() {
        // A self-contained repo (CI checkouts are shallow — never assert
        // against the enclosing repo's history) with commits touching three
        // distinct paths, so `path=` has something real to exclude.
        let base =
            std::env::temp_dir().join(format!("ikigai-repo-log-path-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub")).unwrap();
        let git_in = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", base.to_str().unwrap()])
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git_in(&["init", "-q"]);
        for (file, subject) in [
            ("a.txt", "touch a (#1)"),
            ("sub/b.txt", "touch sub (#2)"),
            ("a.txt", "touch a again (#3)"),
        ] {
            std::fs::write(base.join(file), subject).unwrap();
            git_in(&["add", file]);
            git_in(&["commit", "-q", "-m", subject]);
        }
        let dir = base.to_str().unwrap();
        let git = Capability::scoped(["urn:cap:exec:git"]);

        // path= narrows the text face to the commits that touched the subtree.
        let out = source("urn:repo:log", &[("dir", dir), ("path", "sub")], &git).unwrap();
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 1, "{body:?}");
        assert!(body.contains("touch sub (#2)"), "{body:?}");

        // …and composes with limit=: two a-touching commits, newest first,
        // limit=1 keeps only the newest.
        let out = source(
            "urn:repo:log",
            &[("dir", dir), ("path", "a.txt"), ("limit", "1")],
            &git,
        )
        .unwrap();
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 1, "{body:?}");
        assert!(body.contains("touch a again (#3)"), "{body:?}");

        // The JSON face takes the same pathspec: honest subjects, same scope.
        let out = source(
            "urn:repo:log",
            &[("dir", dir), ("path", "a.txt"), ("as", "application/json")],
            &git,
        )
        .unwrap();
        assert_eq!(out.repr_type.media_type, "application/json");
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.matches("\"subject\":").count(), 2, "{body:?}");
        assert!(body.contains("touch a again (#3)"), "{body:?}");
        assert!(!body.contains("touch sub"), "{body:?}");

        // A path that looks like a flag is refused before git runs — it could
        // never act as one (it rides after `--`), but silence would be worse.
        let err = source("urn:repo:log", &[("dir", dir), ("path", "--all")], &git).unwrap_err();
        assert!(
            format!("{err:?}").contains("path must be a pathspec"),
            "{err:?}"
        );
        // The empty pathspec is refused with our error, not git's fatal.
        let err = source("urn:repo:log", &[("dir", dir), ("path", "")], &git).unwrap_err();
        assert!(
            format!("{err:?}").contains("path must not be empty"),
            "{err:?}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The `gh pr view --json files` argv is the contract with gh: both faces
    /// share one export, the text face rendering it as one path per line.
    #[test]
    fn pr_files_builds_the_gh_invocation() {
        // Text face: the export plus the per-path template.
        let args = pr_files_args(7, false, None);
        assert_eq!(args[..5], ["pr", "view", "7", "--json", "files"]);
        let at = args.iter().position(|a| a == "--template").unwrap();
        assert_eq!(args[at + 1], PR_FILES_TEMPLATE);
        assert!(
            PR_FILES_TEMPLATE.contains("{{.path}}"),
            "{PR_FILES_TEMPLATE}"
        );
        assert!(PR_FILES_TEMPLATE.contains('\n'), "one path per line");

        // JSON face: no template — gh's export passes through verbatim.
        // repo= appends.
        let args = pr_files_args(7, true, Some("owner/name"));
        assert!(args.iter().all(|a| a != "--template"), "{args:?}");
        assert_eq!(args[args.len() - 2..], ["--repo", "owner/name"]);
    }

    #[test]
    fn pr_files_is_gated_and_guards_pr() {
        // No exec:gh grant → typed, permanent Denied before any gh runs.
        let bare = Capability::scoped(["urn:cap:exec:git"]);
        let err = source("urn:repo:pr:files", &[("pr", "1")], &bare).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");

        // With the grant but no pr= → a clean missing-argument error.
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        let err = source("urn:repo:pr:files", &[], &gh).unwrap_err();
        assert!(format!("{err:?}").contains("MissingArgument"), "{err:?}");

        // A non-numeric pr= is refused before gh is spawned — the value is
        // positional in the argv, so a stray `--web` would otherwise become a
        // flag to gh. The same guard now fronts the whole pr:* family.
        for iri in [
            "urn:repo:pr:files",
            "urn:repo:pr:view",
            "urn:repo:pr:diff",
            "urn:repo:pr:checks",
        ] {
            let err = source(iri, &[("pr", "--web")], &gh).unwrap_err();
            assert!(
                format!("{err:?}").contains("pr must be a number"),
                "{iri}: {err:?}"
            );
        }
    }

    #[test]
    fn json_str_escapes() {
        assert_eq!(json_str("plain"), "\"plain\"");
        assert_eq!(json_str("a \"q\" \\ b"), "\"a \\\"q\\\" \\\\ b\"");
        assert_eq!(json_str("nl\ntab\t"), "\"nl\\ntab\\t\"");
        assert_eq!(json_str("bell\u{7}"), "\"bell\\u0007\"");
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
