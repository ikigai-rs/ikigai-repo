//! Git and `gh` as ikigai resources.
//!
//! This module is the **platform seam** for development tooling: a set of
//! **typed facades** (`urn:repo:status`, `:log`, `:show`, `:branch`, and the
//! `urn:repo:pr:*` family: `:list`, `:view`, `:files`, `:diff`, `:checks`), each
//! of which builds a FIXED argument vector for `git` or `gh` and speaks the
//! ikigai self-description. A companion `urn:repo:list` enumerates the
//! repositories under a ROOT so an agent whose cwd is not a repo (e.g.
//! `ikigai mcp`) can discover where to point `dir=`.
//!
//! Native-only by nature (it spawns subprocesses), so it carries no wasm face —
//! it is to the shell what `ikigai-personal` is to EventKit.
//!
//! ## Process bodies
//! [`ikigai_core::Endpoint::invoke_body`] serves `urn:repo:log` (both faces)
//! and `urn:repo:pr:diff` as one-shot bodies. Stdout is read on demand in
//! fixed-size chunks; JSON history retains only the current bounded record.
//! The run's byte budget bounds the streamed answer, and stderr retains the
//! independent [`MAX_OUTPUT_BYTES`] bound. Overflow and a non-zero exit end
//! the body with an error item: a partial answer is never a complete answer.
//! Dropping a body kills and reaps its direct child, including a pending read.
//! The endpoints' bytes `invoke` implementation keeps its existing bounds.
//!
//! ## No low-level exec is served
//! The space binds no endpoint that takes a caller's argument vector. An earlier
//! `urn:system:exec` did (`tool=` + `args=` + `dir=`, gated only by a tool
//! allowlist and `urn:cap:exec:{tool}`), and an argv is not a safe boundary for
//! these tools: git, gh, cargo and just each have flags and configuration that
//! run other programs, so a per-tool grant over a caller-built argv was a grant
//! to run anything (ledger #1706). The facades are the only seam: each fixes the
//! subcommand and flags, and its caller supplies only typed values (a number, a
//! state from a closed vocabulary, a pathspec after `--`, a revision that is
//! not a flag, a directory, an `owner/name`).
//!
//! ## `dir=` is confined to the host's roots
//! A facade pointed with `dir=` at a repository honors that repository's own
//! configuration, as git and gh always do. So the directory is the HOST's
//! choice, not the caller's: a host hands the space the directories it serves
//! ([`space_with_roots`]; gonk passes its browse roots), and a `dir=` that does
//! not resolve to a directory under one of them is refused with a typed
//! [`Error::Denied`] before any process is spawned (ledger #1712). Both sides
//! are canonicalized first, so a `..` or a symlink that leads out of every root
//! is refused like any other outside path. [`space`] configures no roots, so it
//! refuses every `dir=`: the closed default.
//!
//! A confined call also sets `GIT_CEILING_DIRECTORIES` to the parent of the
//! outermost root that holds it, so git's own upward search for a repository
//! stops at that root and never adopts a repository above it.
//!
//! A call WITHOUT `dir=` runs in the process's working directory, which the
//! host chose when it started; a caller cannot move it.
//!
//! ## The verb tells the truth
//! A read (`git status`, `gh pr view`) is a `Source`; a mutation (`git commit`)
//! would be a `Sink` under a write capability. v1 ships the read facades only.
//!
//! ## Capabilities
//! Each facade declares and enforces the concrete capability it needs:
//! `urn:cap:exec:git` for the git facades, `urn:cap:exec:gh` for the
//! `urn:repo:pr:*` family. Those grants now gate only these facades; holding
//! `urn:cap:exec:git` does not let a caller choose what git runs. No grant, no
//! tool.
//!
//! `urn:repo:list` is a filesystem read, not an exec — it enumerates directory
//! names, never spawning a process — so it declares and enforces
//! `urn:cap:fs:read:*` instead of an `exec:*` scope. An explicit `root=` is
//! confined to the host's repository roots just like `dir=`; omitting it uses
//! the host-selected `$IKIGAI_REPO_ROOT`, else `~/git-personal`.

mod process_body;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use ikigai_core::{
    space_iri, ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, ReprType,
    Representation, Result, UriTemplate, Verb,
};

/// The tools [`run`] will spawn. Anything else is refused before a process is
/// created — the allowlist is the outer bound, the capability the inner one.
/// Only `git` and `gh` have facades; `run` is private and every call site
/// passes a fixed tool, so the list is a backstop, not a menu.
const ALLOWED_TOOLS: &[&str] = &["git", "gh"];

/// Maximum raw bytes captured from EACH of stdout and stderr for one git/gh
/// invocation (8 MiB each, 16 MiB combined). Exceeding either limit refuses the
/// entire call with [`Error::OverBudget`]; partial output is never returned.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

fn read_output(reader: impl Read, tool: &str, stream: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_OUTPUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::Endpoint(format!("exec: could not read `{tool}` {stream}: {e}")))?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(Error::OverBudget {
            detail: format!(
                "exec: `{tool}` {stream} exceeds {MAX_OUTPUT_BYTES} bytes (8 MiB); no output returned"
            ),
        });
    }
    Ok(bytes)
}

/// Drain both pipes concurrently: reading one to completion first can deadlock
/// when the child fills the other. A reader reports overflow immediately so we
/// kill and reap the child, rather than continuing to buffer or drain its output.
fn bounded_output(command: &mut Command, tool: &str) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Endpoint(format!("exec: could not run `{tool}`: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (stdout, stderr, error) = std::thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel();
        let err_tx = tx.clone();
        scope.spawn(move || {
            let _ = tx.send((true, read_output(stdout, tool, "stdout")));
        });
        scope.spawn(move || {
            let _ = err_tx.send((false, read_output(stderr, tool, "stderr")));
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut error = None;
        for (is_stdout, result) in rx {
            match result {
                Ok(bytes) if is_stdout => stdout = bytes,
                Ok(bytes) => stderr = bytes,
                Err(err) => {
                    // Preserve the first failure, especially OverBudget, even
                    // when killing the child makes the other pipe fail too.
                    if error.is_none() {
                        error = Some(err);
                        let _ = child.kill();
                    }
                }
            }
        }
        (stdout, stderr, error)
    });
    let status = child.wait();
    if let Some(error) = error {
        return Err(error);
    }
    let status =
        status.map_err(|e| Error::Endpoint(format!("exec: could not wait for `{tool}`: {e}")))?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// The directories a facade may run in, as the host passed them. Shared by
/// every facade of one space; canonicalized per call, never at construction, so
/// building a space reads nothing.
type Roots = Arc<[PathBuf]>;

/// Where a confined call runs: the canonical directory, and the ceiling git's
/// upward repository search must not pass.
struct Place {
    dir: PathBuf,
    ceiling: Option<PathBuf>,
}

/// Confine a caller's `dir=` to `roots`, or refuse it with a typed, permanent
/// [`Error::Denied`]. Pure path work: nothing is spawned here.
///
/// The directory and every root are canonicalized before they are compared, so
/// `..` components and symlinks are resolved first and the comparison is
/// component-wise (`/r/a` is not under `/r/ab`). A directory that does not
/// resolve (absent, unreadable, not a directory) is refused with the same
/// message as one outside every root, so the refusal does not say whether an
/// outside path exists. The ceiling is the parent of the OUTERMOST root that
/// holds the directory, so git may still find a repository at any root above it.
fn confine(roots: &[PathBuf], dir: &str) -> Result<Place> {
    confine_path(roots, "dir", dir)
}

/// Apply the same canonical path boundary to a directory or an explicit list root.
fn confine_path(roots: &[PathBuf], arg: &str, dir: &str) -> Result<Place> {
    if roots.is_empty() {
        return Err(Error::Denied(format!(
            "{arg}=`{dir}` refused: this space has no repository roots configured, so it runs \
             in no caller-named directory; a host passes the directories it serves with \
             ikigai_repo::space_with_roots(..)"
        )));
    }
    let refused = || {
        Error::Denied(format!(
            "{arg}=`{dir}` is not a directory under a configured repository root"
        ))
    };
    // ambient: fs: resolves the caller's directory (links, `..`) to compare with host-configured roots; reached only after the facade's exec grant or the list endpoint's filesystem read grant check
    #[allow(clippy::disallowed_methods)]
    let target = Path::new(dir)
        .canonicalize()
        .ok()
        .filter(|target| target.is_dir())
        .ok_or_else(refused)?;
    // ambient: fs: resolves the host-configured repository roots (never a request name) the same way, so the comparison above is component-wise on real paths
    #[allow(clippy::disallowed_methods)]
    let root = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .filter(|root| target.starts_with(root))
        .min_by_key(|root| root.components().count())
        .ok_or_else(refused)?;
    Ok(Place {
        ceiling: root.parent().map(Path::to_path_buf),
        dir: target,
    })
}

/// Listing the repositories under a ROOT reads directory names — a filesystem
/// read. `urn:repo:list` declares and enforces this scope (a named MCP
/// grant can hold `urn:cap:fs:read:*`). Matching is exact today, so the
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

/// Run an allowlisted tool with an argument vector in `dir`, capability-gated
/// and confined to `roots`. Returns its stdout on success (exit 0); a missing
/// capability or a `dir` outside every root is a typed, permanent
/// [`Error::Denied`], while a non-zero exit or a spawn failure is an
/// [`Error::Endpoint`] carrying stderr — so a caller sees *why*, as data. Every
/// spawn in this crate goes through here, so every authority refusal comes first.
/// Each output pipe is bounded by [`MAX_OUTPUT_BYTES`]; overflow kills and reaps
/// the child and returns [`Error::OverBudget`] without partial output.
fn run(
    inv: &Invocation<'_>,
    tool: &str,
    args: &[String],
    dir: Option<&str>,
    roots: &[PathBuf],
) -> Result<String> {
    let output = run_output(inv, tool, args, dir, roots)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(tool_failed(tool, &output))
    }
}

/// [`run`] up to the exit status, whatever it was, with stdout's raw bytes.
fn run_output(
    inv: &Invocation<'_>,
    tool: &str,
    args: &[String],
    dir: Option<&str>,
    roots: &[PathBuf],
) -> Result<Output> {
    let mut command = prepare_command(inv, tool, args, dir, roots)?;
    bounded_output(&mut command, tool)
}

fn tool_failed(tool: &str, output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    Error::Endpoint(format!(
        "exec: `{tool}` exited {}: {}",
        output.status.code().unwrap_or(-1),
        stderr.trim()
    ))
}

fn prepare_command(
    inv: &Invocation<'_>,
    tool: &str,
    args: &[String],
    dir: Option<&str>,
    roots: &[PathBuf],
) -> Result<Command> {
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
    let place = dir.map(|dir| confine(roots, dir)).transpose()?;
    // ambient: process: running git and gh is this connector's job; the tool is one of ALLOWED_TOOLS, the caller holds `urn:cap:exec:{tool}` (checked above), and its directory is confined to the host's roots
    #[allow(clippy::disallowed_methods)]
    let mut command = Command::new(tool);
    // Tool-generated presentation must stay plain even when the host forces color.
    if tool == "git" {
        command.args(["-c", "color.ui=never"]);
    } else {
        command
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0")
            .env("CLICOLOR_FORCE", "0")
            .env_remove("GH_FORCE_TTY");
    }
    command.args(args);
    if let Some(place) = &place {
        command.current_dir(&place.dir);
        // gh runs git too, so the ceiling binds both tools.
        if let Some(ceiling) = &place.ceiling {
            command.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
    }
    Ok(command)
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

/// The `dir` input every facade declares: a directory under the host's roots.
fn dir_arg(summary: &str) -> ArgSpec {
    ArgSpec::new("dir")
        .class(XSD_STRING)
        .summary(format!(
            "{summary}; must lie under a repository root the host configured, else Denied"
        ))
        .optional()
}

/// A `git`-backed read facade: runs `git <args…>` in the confined `dir`.
fn git_facade(
    id: &'static str,
    title: &'static str,
    summary: &'static str,
    git_args: &'static [&'static str],
    roots: Roots,
) -> FnEndpoint {
    FnEndpoint::new(id, move |inv: &Invocation<'_>| {
        let args: Vec<String> = git_args.iter().map(|a| a.to_string()).collect();
        let dir = inv.inline_str("dir").ok();
        run(inv, "git", &args, dir, &roots).map(text)
    })
    .with_description(
        Description::new(id)
            .title(title)
            .summary(summary)
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:git")
            .input(dir_arg(
                "the repository directory (defaults to the process cwd)",
            ))
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
    roots: Roots,
) -> impl ikigai_core::Endpoint {
    let body_roots = roots.clone();
    let endpoint = FnEndpoint::new(id, move |inv: &Invocation<'_>| {
        let (args, want_json) = gh_pr_args(inv, sub, json_fields)?;
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir, &roots).map(if want_json { json } else { text })
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
            .input(dir_arg("a repo directory to run in (else the process cwd)"));
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
    });
    process_body::StreamingEndpoint {
        inner: endpoint,
        roots: body_roots,
        kind: process_body::Kind::Pr {
            sub,
            enabled: id == "repo-pr-diff",
        },
    }
}

fn gh_pr_args(
    inv: &Invocation<'_>,
    sub: &[&str],
    json_fields: Option<&str>,
) -> Result<(Vec<String>, bool)> {
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
    Ok((args, want_json))
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
fn pr_files(roots: Roots) -> FnEndpoint {
    FnEndpoint::new("repo-pr-files", move |inv: &Invocation<'_>| {
        let pr = parse_pr(inv)?;
        let want_json = wants_json(inv);
        let repo = inv.inline_str("repo").ok();
        let args = pr_files_args(pr, want_json, repo);
        let dir = inv.inline_str("dir").ok();
        run(inv, "gh", &args, dir, &roots).map(if want_json { json } else { text })
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
            .input(dir_arg("a repo directory to run in (else the process cwd)"))
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
fn pr_list(roots: Roots) -> FnEndpoint {
    FnEndpoint::new("repo-pr-list", move |inv: &Invocation<'_>| {
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
        run(inv, "gh", &args, dir, &roots).map(if want_json { json } else { text })
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
            .input(dir_arg("a repo directory to run in (else the process cwd)"))
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
fn log(roots: Roots) -> impl ikigai_core::Endpoint {
    let body_roots = roots.clone();
    let endpoint = FnEndpoint::new("repo-log", move |inv: &Invocation<'_>| {
        let (args, want_json) = log_args(inv)?;
        let raw = run(inv, "git", &args, inv.inline_str("dir").ok(), &roots)?;
        if want_json {
            let mut entries = Vec::new();
            let mut remaining = raw.as_bytes();
            while !remaining.is_empty() {
                let end = log_record_end(remaining).ok_or_else(log_record_error)?;
                let record = std::str::from_utf8(&remaining[..end]).expect("decoded output");
                entries.push(log_entry(record).ok_or_else(log_record_error)?);
                remaining = &remaining[end + 1..];
            }
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
            .input(dir_arg("the repository directory (defaults to the process cwd)"))
            .input(
                ArgSpec::new("as").class(XSD_STRING)
                    .summary("application/json for the structured face")
                    .one_of(["application/json"])
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
            .output("application/json"),
    );
    process_body::StreamingEndpoint {
        inner: endpoint,
        roots: body_roots,
        kind: process_body::Kind::Log,
    }
}

fn log_args(inv: &Invocation<'_>) -> Result<(Vec<String>, bool)> {
    let limit = parse_limit(inv, 20)?;
    let path = parse_path(inv)?;
    let want_json = wants_json(inv);
    let mut args: Vec<String> = vec!["log".to_string()];
    if want_json {
        // Four NUL-terminated fields: names can contain the old unit separator,
        // and line boundaries are not field boundaries. tformat plus -z also
        // terminates the last subject, including an empty one.
        args.push("--pretty=tformat:%H%x00%an%x00%aI%x00%s".to_string());
        args.push("-z".to_string());
    } else {
        args.push("--oneline".to_string());
    }
    args.push(format!("-{limit}"));
    // Both faces interpret path= from the repository root even when dir=
    // names a subdirectory. It still travels after git's path delimiter.
    if let Some(path) = path {
        args.push("--".to_string());
        args.push(root_pathspec(&path));
    }
    Ok((args, want_json))
}

/// Anchor a pathspec at the repository root without losing its magic.
fn root_pathspec(path: &str) -> String {
    if path == ":" {
        path.to_string() // Git's special "no pathspec" spelling.
    } else if let Some(magic) = path.strip_prefix(":(") {
        format!(":(top,{magic}")
    } else if let Some(short) = path.strip_prefix(':') {
        format!(":/{short}")
    } else {
        format!(":(top){path}")
    }
}

/// Index of the fourth NUL, excluding it from the record passed to log_entry.
fn log_record_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .enumerate()
        .filter(|(_, b)| **b == 0)
        .nth(3)
        .map(|(i, _)| i)
}

fn log_record_error() -> Error {
    Error::Endpoint("git log: incomplete structured record; no complete answer returned".into())
}

fn log_entry(line: &str) -> Option<String> {
    let mut parts = line.splitn(4, '\0');
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
}

/// A caller's value as a refusal quotes it: control characters are escaped, so
/// a refusal never carries a terminal control code it was handed.
fn quoted(value: &str) -> String {
    format!("`{}`", value.escape_debug())
}

fn invalid(name: &str, detail: String) -> Error {
    Error::InvalidArgument {
        name: name.to_string(),
        detail,
    }
}

/// Parse the required `rev=` argument. Like [`parse_pr`] this is a flag guard:
/// the value is positional in git's argv, and a `--output=` must never get there.
fn parse_rev<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    let rev = inv.inline_str("rev")?;
    if rev.is_empty() || rev.starts_with('-') || rev.contains('\0') {
        return Err(invalid(
            "rev",
            format!(
                "{} is not a revision: give a commit hash, branch or tag",
                quoted(rev)
            ),
        ));
    }
    Ok(rev)
}

/// Parse the required `path=` argument: a file's name in a commit's tree,
/// relative to the repository's root. A path that a tree cannot hold is refused
/// here, so that what reaches git is only ever a question about the tree.
fn parse_show_path<'a>(inv: &'a Invocation<'_>) -> Result<&'a str> {
    let path = inv.inline_str("path")?;
    let in_tree = !path.is_empty()
        && !path.starts_with(['-', '/'])
        && !path.contains('\0')
        && !path.split('/').any(|segment| matches!(segment, "." | ".."));
    if !in_tree {
        return Err(invalid(
            "path",
            format!(
                "{} is not a file path relative to the repository root",
                quoted(path)
            ),
        ));
    }
    Ok(path)
}

/// The blob `git ls-tree -z` lists under exactly `path`, if there is one. A
/// directory or a submodule is listed too, and it is not a file; `vocab/` lists
/// `vocab`'s children, and `a//b` lists `a/b`, under names that are not `path`.
fn blob_in_listing<'a>(listing: &'a [u8], path: &str) -> Option<&'a str> {
    listing.split(|byte| *byte == 0).find_map(|entry| {
        let tab = entry.iter().position(|byte| *byte == b'\t')?;
        if &entry[tab + 1..] != path.as_bytes() {
            return None;
        }
        let mut fields = std::str::from_utf8(&entry[..tab]).ok()?.split(' ');
        let (_mode, kind, object) = (fields.next()?, fields.next()?, fields.next()?);
        (kind == "blob").then_some(object)
    })
}

/// Object identities are immutable; provenance is relative to retained, reachable
/// history. Hosts can cut the repository's object-history thread after fetching
/// history or rewriting refs to refresh that part of an indefinitely cached answer.
fn object(roots: Roots) -> FnEndpoint {
    FnEndpoint::new("repo-object", move |inv: &Invocation<'_>| {
        object_lookup(inv, &roots)
    })
    .with_description(
        Description::new("repo-object")
            .title("A Git object")
            .summary(
                "A local object by full SHA: commit, tree, blob or tag. For a blob, the first \
                 retained reachable commit and path in reverse topological history \
                 (git log --all --find-object); null if unavailable. Present answers never \
                 expire; absence has a five-second upper bound. dir= selects the repository, \
                 whose directory name must match {repo}.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:git")
            .input(ArgSpec::new("repo").binding().class(XSD_STRING)
                .summary("the repository directory name, in the resource IRI"))
            .input(ArgSpec::new("sha").binding().class(XSD_STRING)
                .summary("a full SHA in the resource IRI: 40 or 64 hexadecimal digits"))
            .input(dir_arg("the repository directory (defaults to the process cwd)"))
            .input(ArgSpec::new("as").class(XSD_STRING)
                .summary("application/json for sha, type and firstAppearance (commit and path, or null)")
                .one_of(["application/json"]).optional())
            .output("text/plain;charset=utf-8")
            .output("application/json"),
    )
}

fn object_lookup(inv: &Invocation<'_>, roots: &[PathBuf]) -> Result<Representation> {
    if !inv.capability.allows("urn:cap:exec:git") {
        return Err(Error::Denied(
            "exec: capability does not grant `urn:cap:exec:git`".into(),
        ));
    }
    let sha = inv
        .bindings
        .get("sha")
        .ok_or_else(|| Error::MissingArgument("sha".into()))?;
    if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid(
            "sha",
            format!(
                "{} must be a full SHA: 40 or 64 hexadecimal digits",
                quoted(sha)
            ),
        ));
    }
    let sha = sha.to_ascii_lowercase();
    let repo = inv
        .bindings
        .get("repo")
        .ok_or_else(|| Error::MissingArgument("repo".into()))?;
    let dir = inv.inline_str("dir").ok();
    let git = |args: &[&str]| {
        let mut fixed = vec!["--no-replace-objects".to_string()];
        fixed.extend(args.iter().map(|a| a.to_string()));
        let mut command = prepare_command(inv, "git", &fixed, dir, roots)?;
        // A local lookup must not fetch a promisor's missing objects.
        command.env("GIT_NO_LAZY_FETCH", "1");
        let output = bounded_output(&mut command, "git")?;
        Ok::<_, Error>(output)
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    if !top.status.success() {
        return Err(tool_failed("git", &top));
    }
    let top = String::from_utf8_lossy(&top.stdout);
    if Path::new(top.strip_suffix('\n').unwrap_or(&top))
        .file_name()
        .and_then(|s| s.to_str())
        != Some(repo)
    {
        return Err(invalid(
            "repo",
            format!(
                "{} must name the repository directory at dir= or the process cwd",
                quoted(repo)
            ),
        ));
    }
    let resolved = git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("{sha}^{{object}}"),
    ])?;
    if !resolved.status.success() && resolved.status.code() != Some(1) {
        return Err(tool_failed("git", &resolved));
    }
    // In a SHA-1 repository, 64 hex digits can name a ref; in a SHA-256
    // repository, 40 can abbreviate an object. Neither is the requested object.
    if !resolved.status.success()
        || resolved
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&resolved.stdout)
            != sha.as_bytes()
    {
        inv.failure_expires_in(std::time::Duration::from_secs(5));
        return Err(Error::NotFound(format!(
            "sha {} is not held in this repository (absence expires after 5 seconds)",
            quoted(&sha)
        )));
    }
    let kind = git(&["cat-file", "-t", &sha])?;
    if !kind.status.success() {
        return Err(tool_failed("git", &kind));
    }
    let kind = String::from_utf8_lossy(&kind.stdout);
    let kind = kind.trim();
    let mut introduction = None;
    if kind == "blob" {
        let history = git(&[
            "log",
            "--all",
            "--reverse",
            "--topo-order",
            "--root",
            "-m",
            "--format=%H",
            &format!("--find-object={sha}"),
            "--",
        ])?;
        if !history.status.success() {
            return Err(tool_failed("git", &history));
        }
        for commit in String::from_utf8_lossy(&history.stdout).lines() {
            let tree = git(&["ls-tree", "-r", "--full-tree", "-z", commit])?;
            if !tree.status.success() {
                return Err(tool_failed("git", &tree));
            }
            let path = tree.stdout.split(|b| *b == 0).find_map(|entry| {
                let tab = entry.iter().position(|b| *b == b'\t')?;
                let header = std::str::from_utf8(&entry[..tab]).ok()?;
                (header.split(' ').nth(2) == Some(sha.as_str())).then_some(&entry[tab + 1..])
            });
            if let Some(path) = path {
                let path = std::str::from_utf8(path).map_err(|_| {
                    Error::Endpoint("object path is not UTF-8; no output returned".into())
                })?;
                introduction = Some((commit.to_string(), path.to_string()));
                break;
            }
        }
    }
    let repr = if wants_json(inv) {
        let first = introduction
            .as_ref()
            .map(|(commit, path)| {
                format!(
                    "{{\"commit\":{},\"path\":{}}}",
                    json_str(commit),
                    json_str(path)
                )
            })
            .unwrap_or_else(|| "null".into());
        json(format!(
            "{{\"sha\":{},\"type\":{},\"firstAppearance\":{first}}}",
            json_str(&sha),
            json_str(kind)
        ))
    } else {
        let mut body = format!("SHA: {sha}\nType: {kind}\n");
        if let Some((commit, path)) = &introduction {
            body.push_str(&format!(
                "First commit: {commit}\nPath (JSON string): {}\n",
                json_str(path)
            ));
        } else if kind == "blob" {
            body.push_str("First appearance: not found in reachable history\n");
        }
        text(body)
    };
    Ok(repr
        .cacheable()
        .depends_on(format!("urn:repo:{repo}:object-history")))
}

/// `urn:repo:show` — a file's bytes at a commit, or [`Error::NotFound`] if the
/// rev names no commit or the path is not a file there.
///
/// Three fixed steps rather than `git show <rev>:<path>`, because a miss has to
/// be told from a failure: as-of resolution (`ikigai-name`'s `repo_history`) reads
/// `NotFound` as "it did not exist then", so an unreadable repository must not
/// answer it, and git's exit status says which only for `rev-parse --quiet` and an
/// empty listing. The steps also keep a value in its own place. The rev is resolved
/// alone, so a `:` in it cannot choose the file. The path is matched literally from
/// the root (`--literal-pathspecs --full-tree`), never as a pattern. The blob is read
/// by its hash with `cat-file`, which runs no textconv or filter a repository's
/// configuration names, and has no `--output=`.
///
/// The answer is a repository's raw bytes, which may not be text, so they are never
/// decoded; git records no media type, so it is `application/octet-stream`. Collected
/// under [`MAX_OUTPUT_BYTES`], not streamed: a file is refused whole past that limit.
/// Live like the other revision-based reads, even for a full hash.
fn show(roots: Roots) -> FnEndpoint {
    FnEndpoint::new("repo-show", move |inv: &Invocation<'_>| {
        let rev = parse_rev(inv)?;
        let path = parse_show_path(inv)?;
        let dir = inv.inline_str("dir").ok();
        let git = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            run_output(inv, "git", &args, dir, &roots)
        };

        let commit = format!("{rev}^{{commit}}");
        let resolved = git(&["rev-parse", "--verify", "--quiet", &commit])?;
        if !resolved.status.success() {
            // `--quiet` makes 1 the exit for "no such commit"; anything else, such
            // as 128 for "not a git repository", is git failing, not a miss.
            return Err(if resolved.status.code() == Some(1) {
                Error::NotFound(format!(
                    "rev {} does not name a commit in this repository",
                    quoted(rev)
                ))
            } else {
                tool_failed("git", &resolved)
            });
        }
        let stdout = String::from_utf8_lossy(&resolved.stdout);
        let commit = stdout.trim();

        let listing = git(&[
            "--literal-pathspecs",
            "ls-tree",
            "--full-tree",
            "-z",
            commit,
            "--",
            path,
        ])?;
        if !listing.status.success() {
            return Err(tool_failed("git", &listing));
        }
        let Some(blob) = blob_in_listing(&listing.stdout, path) else {
            return Err(Error::NotFound(format!(
                "path {} is not a file at commit {commit}",
                quoted(path)
            )));
        };

        let file = git(&["cat-file", "blob", blob])?;
        if !file.status.success() {
            return Err(tool_failed("git", &file));
        }
        Ok(Representation::new(
            ReprType::new("application/octet-stream"),
            file.stdout,
        ))
    })
    .with_description(
        Description::new("repo-show")
            .title("A file at a commit")
            .summary(
                "The bytes of one file as it stood at a commit (git cat-file), unchanged. \
                 NotFound if rev= names no commit or path= is not a file at that commit. Refused \
                 whole past the 8 MiB output limit. Pair with urn:repo:log path= to pick the \
                 commit.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires("urn:cap:exec:git")
            .input(
                ArgSpec::new("rev")
                    .class(XSD_STRING)
                    .summary("the commit to read from: a commit hash, branch or tag"),
            )
            .input(
                ArgSpec::new("path")
                    .class(XSD_STRING)
                    .summary("the file to read, relative to the repository root, taken literally"),
            )
            .input(dir_arg(
                "the repository directory (defaults to the process cwd)",
            ))
            .output("application/octet-stream"),
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

/// The repositories directly under `root`: `(name, absolute path)` for each
/// immediate child directory that holds a `.git` entry, unsorted.
///
/// One level deep only, and nothing is opened: it reads directory names and
/// probes for `.git`. Non-UTF-8 names are skipped rather than mangled.
// ambient: fs: urn:repo:list's directory scan, reached only after that endpoint's `urn:cap:fs:read:*` check; it reads the child names of its `root=` (else IKIGAI_REPO_ROOT, else ~/git-personal) and probes each for `.git`
#[allow(clippy::disallowed_methods)]
fn repos_under(root: &Path) -> std::io::Result<Vec<(String, String)>> {
    let mut repos = Vec::new();
    for entry in std::fs::read_dir(root)?.flatten() {
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
    Ok(repos)
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
fn list(roots: Roots) -> FnEndpoint {
    FnEndpoint::new("repo-list", move |inv: &Invocation<'_>| {
        if !inv.capability.allows(FS_READ) {
            // Typed `Denied` — a permanent authority failure the trace, manifold,
            // and wire recognize as a 403-equivalent without sniffing message text.
            return Err(Error::Denied(format!(
                "repo-list: capability does not grant `{FS_READ}`"
            )));
        }
        let root = if let Ok(root) = inv.inline_str("root") {
            confine_path(&roots, "root", root)?.dir
        } else {
            repo_root(inv)?
        };
        let mut repos = repos_under(&root).map_err(|e| {
            Error::Endpoint(format!(
                "repo-list: cannot read root `{}`: {e}",
                root.display()
            ))
        })?;
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
                 else $IKIGAI_REPO_ROOT, else ~/git-personal; immediate child directories only. \
                 An explicit root= must resolve under a host-configured repository root.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(FS_READ)
            .input(
                ArgSpec::new("root")
                    .class(XSD_STRING)
                    .summary(
                        "the directory to scan, within a configured repository root \
                         (default: $IKIGAI_REPO_ROOT, else ~/git-personal)",
                    )
                    .optional(),
            )
            .output("text/plain;charset=utf-8"),
    )
}

/// The name [`space`] claims: `urn:iki:space:repo`.
pub const SPACE_ID: &str = "urn:iki:space:repo";

/// The dev-tooling space with NO repository roots: the closed default.
///
/// Every `dir=` is refused with a typed [`Error::Denied`] that names
/// [`space_with_roots`]; a call without `dir=` still runs in the process's
/// working directory, and `urn:repo:list` without `root=` uses its host-selected default.
/// An explicit `root=` is refused too. It serves no low-level
/// exec: `urn:system:exec` is not bound, so the kernel answers it `Unresolved`
/// (ledger #1706).
///
/// Configuration-free, so it names itself [`SPACE_ID`]: building it reads
/// nothing. `$IKIGAI_REPO_ROOT`, `$HOME`, the process cwd and the tools on
/// `PATH` are read per invocation, never here.
pub fn space() -> EndpointSpace {
    // Named last: a later `bind` drops the name (core 0.1.89).
    bind(Arc::from(Vec::new())).named(space_iri("repo"))
}

/// The dev-tooling space whose facades run in any directory under `roots`
/// (ledger #1712): the directories the host serves, such as gonk's browse roots.
/// A `dir=` resolving outside all of them, by a plain path, a `..` or a
/// symlink, is refused with a typed [`Error::Denied`] before a process is
/// spawned. Explicit list `root=` paths use the same boundary. An empty
/// `roots` refuses every `dir=` and explicit `root=`, as [`space`] does.
///
/// Roots are canonicalized on each call, not here, so building the space reads
/// nothing and a root created later is honored. A root that does not resolve
/// matches nothing.
///
/// Built from what the host passed, so it claims no name: the host names it
/// (core's rule for instance-built spaces, ledger #987).
///
/// ```
/// use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Space, Verb};
/// use std::sync::Arc;
///
/// let root = std::env::temp_dir().join(format!("ikigai-repo-doc-{}", std::process::id()));
/// std::fs::create_dir_all(&root).unwrap();
/// let space = ikigai_repo::space_with_roots([&root]);
/// assert!(space.id().is_none(), "the host names an instance-built space");
///
/// // A directory outside the root is refused before git is spawned.
/// let kernel = Kernel::new(Arc::new(space));
/// let outside = root.parent().unwrap().to_str().unwrap().as_bytes().to_vec();
/// let request = Request::new(Verb::Source, Iri::parse("urn:repo:status").unwrap())
///     .with_arg("dir", ArgRef::Inline(outside));
/// let capability = Capability::scoped(["urn:cap:exec:git"]);
/// let err = futures::executor::block_on(kernel.issue(request, &capability)).unwrap_err();
/// assert!(matches!(err, Error::Denied(_)), "{err:?}");
/// std::fs::remove_dir_all(&root).ok();
/// ```
pub fn space_with_roots<I, P>(roots: I) -> EndpointSpace
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let roots: Vec<PathBuf> = roots
        .into_iter()
        .map(|root| root.as_ref().to_path_buf())
        .collect();
    bind(Arc::from(roots))
}

/// Bind every endpoint over one shared set of roots. Unnamed: [`space`] names
/// its result, a host names [`space_with_roots`]'s.
fn bind(roots: Roots) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new("urn:repo:status"),
            git_facade(
                "repo-status",
                "Repository status",
                "The working tree's status, machine-readable (git status --porcelain=v1 -b).",
                &["status", "--porcelain=v1", "-b"],
                roots.clone(),
            ),
        )
        .bind(Exact::new("urn:repo:log"), log(roots.clone()))
        .bind(Exact::new("urn:repo:show"), show(roots.clone()))
        .bind(
            UriTemplate::parse("urn:repo:{repo}:object:sha:{sha}").expect("fixed object template"),
            object(roots.clone()),
        )
        .bind(
            Exact::new("urn:repo:branch"),
            git_facade(
                "repo-branch",
                "Current branch",
                "The current branch name (git branch --show-current).",
                &["branch", "--show-current"],
                roots.clone(),
            ),
        )
        .bind(Exact::new("urn:repo:list"), list(roots.clone()))
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
                roots.clone(),
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
                roots.clone(),
            ),
        )
        .bind(Exact::new("urn:repo:pr:list"), pr_list(roots.clone()))
        .bind(Exact::new("urn:repo:pr:files"), pr_files(roots.clone()))
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
                roots,
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
        issue_on(&kernel(), iri, args, cap)
    }

    /// Issue a Source on a kernel over [`space_with_roots`]`(roots)`.
    fn source_under(
        roots: &[&std::path::Path],
        iri: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<Representation> {
        let kernel = Kernel::new(Arc::new(space_with_roots(roots)));
        issue_on(&kernel, iri, args, cap)
    }

    fn issue_on(
        kernel: &Kernel,
        iri: &str,
        args: &[(&str, &str)],
        cap: &Capability,
    ) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap());
        for (k, v) in args {
            request = request.with_arg(*k, ArgRef::Inline(v.as_bytes().to_vec()));
        }
        block_on(kernel.issue(request, cap))
    }

    fn no_roots() -> Roots {
        Arc::from(Vec::new())
    }

    /// The space serves no low-level exec (ledger #1706): `urn:system:exec` is
    /// unbound, so the kernel answers it with the typed `Unresolved` it gives any
    /// name nothing binds — under the root capability and under the per-tool
    /// grant alike, so no grant reaches a caller-built argv.
    #[test]
    fn system_exec_is_not_served() {
        for cap in [
            Capability::root(),
            Capability::scoped(["urn:cap:exec:git", "urn:cap:exec:gh", "urn:cap:exec:*"]),
        ] {
            let err = source(
                "urn:system:exec",
                &[("tool", "git"), ("args", "--version")],
                &cap,
            )
            .unwrap_err();
            assert!(matches!(err, Error::Unresolved(_)), "{err:?}");
        }
    }

    /// Call the private [`run`] with a FIXED tool and argv, as a facade does,
    /// through a one-endpoint kernel (an `Invocation` is the kernel's to build).
    /// No description, so nothing but `run` itself gates the call.
    fn run_fixed(
        tool: &'static str,
        args: &'static [&'static str],
        cap: &Capability,
    ) -> Result<Representation> {
        let probe = FnEndpoint::new("run-probe", move |inv: &Invocation<'_>| {
            let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            run(inv, tool, &args, None, &[]).map(text)
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:run"), probe),
        ));
        let request = Request::new(Verb::Source, Iri::parse("urn:test:run").unwrap());
        block_on(kernel.issue(request, cap))
    }

    #[test]
    fn output_reader_accepts_exact_limit_and_refuses_next_byte() {
        for stream in ["stdout", "stderr"] {
            let bytes = vec![b'x'; MAX_OUTPUT_BYTES];
            assert_eq!(read_output(bytes.as_slice(), "git", stream).unwrap(), bytes);
            let err = read_output(
                std::io::repeat(b'x').take(MAX_OUTPUT_BYTES as u64 + 1),
                "gh",
                stream,
            )
            .unwrap_err();
            assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
            assert!(!err.is_transient());
            assert!(err.to_string().contains(stream));
            assert!(err.to_string().contains(&MAX_OUTPUT_BYTES.to_string()));
        }
    }

    #[cfg(unix)]
    #[test]
    fn output_capture_drains_both_pipes_and_preserves_exit_status() {
        let output = bounded_output(
            Command::new("sh").args(["-c", &format!(
                "head -c {MAX_OUTPUT_BYTES} /dev/zero; head -c {MAX_OUTPUT_BYTES} /dev/zero >&2; exit 7"
            )]), "fixture",
        ).unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, vec![0; MAX_OUTPUT_BYTES]);
        assert_eq!(output.stderr, vec![0; MAX_OUTPUT_BYTES]);
    }

    #[cfg(unix)]
    #[test]
    fn output_capture_kills_unending_stdout_or_stderr_on_overflow() {
        for script in ["exec yes x", "exec yes x >&2"] {
            let err =
                bounded_output(Command::new("sh").args(["-c", script]), "fixture").unwrap_err();
            assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
        }
    }

    #[test]
    fn run_refuses_stdout_one_byte_over_budget() {
        // A real git blob: the bytes reaching run are exactly the file bytes.
        let base = std::env::current_dir()
            .unwrap()
            .join("target/output-budget-test");
        std::fs::create_dir_all(&base).unwrap();
        assert!(Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&base)
            .status()
            .unwrap()
            .success());
        let bytes = vec![b'x'; MAX_OUTPUT_BYTES + 1];
        std::fs::write(base.join("blob"), &bytes).unwrap();
        let hash = Command::new("git")
            .arg("-C")
            .arg(&base)
            .args(["hash-object", "-w", "blob"])
            .output()
            .unwrap();
        assert!(hash.status.success());
        let hash = String::from_utf8(hash.stdout).unwrap().trim().to_string();
        let probe = FnEndpoint::new("budget-probe", move |inv: &Invocation<'_>| {
            run(
                inv,
                "git",
                &[
                    "-C".into(),
                    base.to_str().unwrap().into(),
                    "cat-file".into(),
                    "blob".into(),
                    hash.clone(),
                ],
                None,
                &[],
            )
            .map(text)
        });
        let kernel = Kernel::new(Arc::new(
            EndpointSpace::new().bind(Exact::new("urn:test:budget"), probe),
        ));
        let result = issue_on(
            &kernel,
            "urn:test:budget",
            &[],
            &Capability::scoped(["urn:cap:exec:git"]),
        );
        assert!(
            matches!(result, Err(Error::OverBudget { .. })),
            "git output one byte over 8 MiB must be refused, got {}",
            if result.is_ok() {
                "success"
            } else {
                "another error"
            }
        );
    }

    /// The gate every facade rides: `run` refuses a tool off the allowlist and a
    /// caller without `urn:cap:exec:{tool}`, both before a process is spawned.
    /// (Moved from the retired `urn:system:exec` test, which pinned the same two
    /// refusals through that endpoint.)
    #[test]
    fn run_is_capability_gated_and_allowlisted() {
        // No exec grant at all → denied. A capability denial is the typed,
        // permanent `Denied` — never a generic `Endpoint` string, and never
        // transient (re-issuing under the same capability won't change the answer).
        let none = Capability::scoped(["urn:cap:unrelated"]);
        let err = run_fixed("git", &["--version"], &none).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");
        assert!(!err.is_transient(), "{err:?}");

        // A grant for one tool does not reach another.
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        let err = run_fixed("git", &["--version"], &gh).unwrap_err();
        assert!(matches!(err, Error::Denied(_)), "{err:?}");

        // A non-allowlisted tool → refused before any process, even under a matching cap.
        let other = Capability::scoped(["urn:cap:exec:rm"]);
        let err = run_fixed("rm", &[], &other).unwrap_err();
        assert!(
            format!("{err:?}").contains("not an allowlisted tool"),
            "{err:?}"
        );

        // cargo and just have no facade, so they are off the list too (ledger
        // #1712): a `--version` that would succeed is refused under the grant.
        for tool in ["cargo", "just"] {
            let cap = Capability::scoped([format!("urn:cap:exec:{tool}")]);
            let err = run_fixed(tool, &["--version"], &cap).unwrap_err();
            assert!(
                format!("{err:?}").contains("not an allowlisted tool"),
                "{tool}: {err:?}"
            );
        }

        // git --version under the git grant → runs, stdout carries "git version".
        let git = Capability::scoped(["urn:cap:exec:git"]);
        let out = run_fixed("git", &["--version"], &git).unwrap();
        assert!(
            String::from_utf8_lossy(&out.bytes).contains("git version"),
            "{:?}",
            String::from_utf8_lossy(&out.bytes)
        );
    }

    /// A fresh, empty base directory for one confinement test, removed on drop.
    struct Base(std::path::PathBuf);

    impl Base {
        fn new(name: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "ikigai-repo-{name}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Base(path)
        }

        /// A plain directory under the base.
        fn dir(&self, rel: &str) -> std::path::PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        }

        /// A hermetic git repository with one commit under the base: a place
        /// where `git status` WOULD answer, so only a refusal keeps it from
        /// answering.
        fn repo(&self, rel: &str) -> std::path::PathBuf {
            let path = self.dir(rel);
            for args in [
                &["init", "-q"][..],
                &["commit", "-q", "--allow-empty", "-m", "scratch"][..],
            ] {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(&path)
                    .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "git {args:?}: {out:?}");
            }
            path
        }
    }

    impl Drop for Base {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_cap() -> Capability {
        Capability::scoped(["urn:cap:exec:git"])
    }

    fn assert_denied(result: Result<Representation>, what: &str) {
        let err = result.err().unwrap_or_else(|| panic!("{what}: answered"));
        assert!(matches!(err, Error::Denied(_)), "{what}: {err:?}");
        assert!(!err.is_transient(), "{what}: {err:?}");
    }

    /// The three git facades, each with the minimal arguments it runs on.
    const GIT_IRIS: [&str; 3] = ["urn:repo:status", "urn:repo:branch", "urn:repo:log"];

    /// A scratch repository with a dated history of one file, for `urn:repo:show`.
    struct History {
        _base: Base,
        root: std::path::PathBuf,
        repo: std::path::PathBuf,
    }

    impl History {
        fn new(name: &str) -> Self {
            let base = Base::new(name);
            let root = base.dir("root");
            let repo = base.repo("root/repo");
            History {
                _base: base,
                root,
                repo,
            }
        }

        fn dir(&self) -> &str {
            self.repo.to_str().unwrap()
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.repo)
                .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }

        /// Commit `bytes` at `path`, answering the commit's full hash.
        fn commit(&self, path: &str, bytes: &[u8]) -> String {
            let file = self.repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
            self.git(&["add", "--", path]);
            self.git(&["commit", "-q", "-m", path]);
            self.git(&["rev-parse", "HEAD"])
        }

        fn show(&self, rev: &str, path: &str) -> Result<Representation> {
            self.show_as(rev, path, &git_cap())
        }

        fn show_as(&self, rev: &str, path: &str, cap: &Capability) -> Result<Representation> {
            source_under(
                &[self.root.as_path()],
                "urn:repo:show",
                &[("dir", self.dir()), ("rev", rev), ("path", path)],
                cap,
            )
        }
    }

    #[test]
    fn object_lookup_resolves_a_full_commit_sha() {
        let repo = History::new("object-reproduction");
        let sha = repo.commit("file.txt", b"hello\n");
        let out = source_under(
            &[repo.root.as_path()],
            &format!("urn:repo:repo:object:sha:{sha}"),
            &[("dir", repo.dir()), ("as", "application/json")],
            &git_cap(),
        )
        .unwrap();
        let body = String::from_utf8(out.bytes).unwrap();
        assert!(body.contains("\"type\":\"commit\""), "{body}");
    }

    impl History {
        fn object(&self, sha: &str, face: &str) -> Result<Representation> {
            source_under(
                &[self.root.as_path()],
                &format!("urn:repo:repo:object:sha:{sha}"),
                &[("dir", self.dir()), ("as", face)],
                &git_cap(),
            )
        }
    }

    #[test]
    fn object_types_and_faces_in_a_scratch_repository() {
        let repo = History::new("object-types");
        let path = "sub/quote\"\t\n.txt";
        let commit = repo.commit(path, b"original\n");
        let blob = repo.git(&["rev-parse", &format!("HEAD:{path}")]);
        let tree = repo.git(&["rev-parse", "HEAD^{tree}"]);
        repo.git(&[
            "-c",
            "tag.gpgSign=false",
            "tag",
            "-a",
            "v1",
            "-m",
            "annotated",
        ]);
        let tag = repo.git(&["rev-parse", "v1"]);
        // Removal and later reuse must not replace the original introduction.
        repo.commit(path, b"replacement\n");
        repo.commit("later.txt", b"original\n");
        for (sha, kind) in [
            (&commit, "commit"),
            (&tree, "tree"),
            (&tag, "tag"),
            (&blob, "blob"),
        ] {
            let out = repo.object(sha, "application/json").unwrap();
            assert_eq!(out.repr_type.media_type, "application/json");
            assert_eq!(out.expiry, ikigai_core::Expiry::Never);
            let first = if kind == "blob" {
                format!(
                    "{{\"commit\":{},\"path\":{}}}",
                    json_str(&commit),
                    json_str(path)
                )
            } else {
                "null".into()
            };
            assert_eq!(
                String::from_utf8(out.bytes).unwrap(),
                format!(
                    "{{\"sha\":{},\"type\":{},\"firstAppearance\":{first}}}",
                    json_str(sha),
                    json_str(kind)
                )
            );
            let out = repo.object(&sha.to_uppercase(), "text/plain").unwrap();
            assert!(out.repr_type.media_type.starts_with("text/plain"));
            let body = String::from_utf8(out.bytes).unwrap();
            assert!(
                body.contains(&format!("SHA: {sha}\nType: {kind}\n")),
                "{body}"
            );
            if kind == "blob" {
                assert!(
                    body.contains(&format!(
                        "First commit: {commit}\nPath (JSON string): {}\n",
                        json_str(path)
                    )),
                    "{body}"
                );
                assert!(!body.contains('\t'));
            }
        }
        // A stored object need not occur in any reachable commit.
        std::fs::write(repo.repo.join("untracked"), b"unreferenced").unwrap();
        let dangling = repo.git(&["hash-object", "-w", "untracked"]);
        let out = repo.object(&dangling, "application/json").unwrap();
        assert!(String::from_utf8(out.bytes)
            .unwrap()
            .contains("\"firstAppearance\":null"));
    }

    #[test]
    fn object_finds_a_blob_introduced_by_a_merge() {
        let repo = History::new("object-merge");
        repo.commit("file", b"base");
        let main = repo.git(&["symbolic-ref", "--short", "HEAD"]);
        repo.git(&["checkout", "-q", "-b", "side"]);
        repo.commit("side", b"side");
        repo.git(&["checkout", "-q", &main]);
        repo.commit("main", b"main");
        repo.git(&["merge", "--no-commit", "--no-ff", "side"]);
        let merge = repo.commit("only-in-merge", b"merge blob");
        let blob = repo.git(&["rev-parse", "HEAD:only-in-merge"]);
        let out = repo.object(&blob, "application/json").unwrap();
        let body = String::from_utf8(out.bytes).unwrap();
        assert!(body.contains(&format!("\"commit\":\"{merge}\"")), "{body}");
        assert!(body.contains("\"path\":\"only-in-merge\""), "{body}");
    }

    #[test]
    fn object_refuses_malformed_sha_and_wrong_repository() {
        let repo = History::new("object-invalid");
        let sha = repo.commit("file", b"data");
        for bad in [
            "abc",
            "HEAD",
            "--all",
            &"g".repeat(40),
            &"a".repeat(39),
            &"a".repeat(41),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            let err = repo.object(bad, "application/json").unwrap_err();
            match err {
                Error::InvalidArgument { name, detail } => {
                    assert_eq!(name, "sha");
                    assert!(detail.contains("40 or 64 hexadecimal digits"), "{detail}");
                }
                err => panic!("{bad}: {err:?}"),
            }
        }
        let err = source_under(
            &[&repo.root],
            &format!("urn:repo:other:object:sha:{sha}"),
            &[("dir", repo.dir())],
            &git_cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { name, .. } if name == "repo"));
        for absent in ["0".repeat(40), "0".repeat(64)] {
            let err = repo.object(&absent, "application/json").unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        }
    }

    #[test]
    fn object_does_not_resolve_a_sha_shaped_ref_as_an_object() {
        let repo = History::new("object-ref");
        repo.commit("file", b"data");
        for length in [40, 64] {
            let absent = "0".repeat(length);
            repo.git(&["branch", &absent]);
            let err = repo.object(&absent, "application/json").unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        }
    }

    #[test]
    fn object_is_gated_in_the_kernel_and_endpoint() {
        use ikigai_core::{Bindings, Endpoint};
        let repo = History::new("object-gated");
        let sha = repo.commit("file", b"data");
        let none = Capability::scoped(Vec::<String>::new());
        let iri = format!("urn:repo:repo:object:sha:{sha}");
        assert_denied(
            source_under(&[&repo.root], &iri, &[("dir", repo.dir())], &none),
            "object",
        );
        assert_denied(
            source_under(&[], &iri, &[("dir", repo.dir())], &git_cap()),
            "no roots",
        );
        let endpoint = object(no_roots());
        assert!(endpoint
            .describe()
            .requires
            .contains(&"urn:cap:exec:git".into()));
        let request = Request::new(Verb::Source, Iri::parse(&iri).unwrap());
        let mut bindings = Bindings::new();
        bindings.insert("repo", "repo");
        bindings.insert("sha", &sha);
        let inv = Invocation::detached(&request, &bindings, &none);
        assert_denied(block_on(endpoint.invoke(&inv)), "direct endpoint");
    }

    #[test]
    fn object_absence_expires_and_presence_is_cached() {
        use ikigai_core::{Clock, Time};
        use std::sync::atomic::{AtomicU64, Ordering};
        struct TestClock(AtomicU64);
        impl Clock for TestClock {
            fn now(&self) -> Time {
                Time(self.0.load(Ordering::SeqCst))
            }
        }
        let repo = History::new("object-cache");
        repo.commit("file", b"committed");
        std::fs::write(repo.repo.join("new-blob"), b"new object").unwrap();
        let sha = repo.git(&["hash-object", "new-blob"]);
        let iri = format!("urn:repo:repo:object:sha:{sha}");
        let clock = Arc::new(TestClock(AtomicU64::new(1_000)));
        let kernel = Kernel::new(Arc::new(space_with_roots([&repo.root])))
            .with_clock(clock.clone())
            .with_failure_ceiling(std::time::Duration::from_secs(60));
        let args = [("dir", repo.dir()), ("as", "application/json")];
        assert!(matches!(
            issue_on(&kernel, &iri, &args, &git_cap()),
            Err(Error::NotFound(_))
        ));
        // Core 0.3 does not cache NotFound at all: its zero lifetime is
        // stricter than the endpoint's stated five-second upper bound.
        assert_eq!(kernel.cache_len(), 0);
        clock.0.store(5_999, Ordering::SeqCst);
        repo.git(&["hash-object", "-w", "new-blob"]);
        let first = issue_on(&kernel, &iri, &args, &git_cap()).unwrap();
        assert_eq!(first.expiry, ikigai_core::Expiry::Never);
        clock.0.store(u64::MAX / 2, Ordering::SeqCst);
        // Even after local removal, the successful immutable answer remains cached.
        std::fs::remove_file(
            repo.repo
                .join(".git/objects")
                .join(&sha[..2])
                .join(&sha[2..]),
        )
        .unwrap();
        let again = issue_on(&kernel, &iri, &args, &git_cap()).unwrap();
        assert_eq!(first.bytes, again.bytes);
        let none = Capability::scoped(Vec::<String>::new());
        assert_denied(issue_on(&kernel, &iri, &args, &none), "cached object");
    }

    #[test]
    fn object_supports_sha256_repositories() {
        let repo = History::new("object-sha256");
        // Reinitialize before creating any objects; Git supports SHA-256 in CI.
        std::fs::remove_dir_all(repo.repo.join(".git")).unwrap();
        repo.git(&["init", "-q", "--object-format=sha256"]);
        let sha = repo.commit("file", b"sha256");
        assert_eq!(sha.len(), 64);
        let err = repo.object(&sha[..40], "application/json").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        let blob = repo.git(&["rev-parse", "HEAD:file"]);
        let out = repo.object(&blob, "application/json").unwrap();
        assert!(String::from_utf8(out.bytes)
            .unwrap()
            .contains(&format!("\"commit\":\"{sha}\"")));
    }

    /// The file as it stood at each commit, not as it stands now.
    #[test]
    fn show_answers_the_file_as_it_stood_at_the_commit() {
        let repo = History::new("show-at");
        let first = repo.commit("vocab/core.ttl", b"version 1\n");
        let second = repo.commit("vocab/core.ttl", b"version 2\n");
        std::fs::write(repo.repo.join("vocab/core.ttl"), b"uncommitted\n").unwrap();
        for (rev, want) in [
            (first.as_str(), &b"version 1\n"[..]),
            (second.as_str(), &b"version 2\n"[..]),
            ("HEAD", &b"version 2\n"[..]),
            ("HEAD~1", &b"version 1\n"[..]),
        ] {
            let out = repo.show(rev, "vocab/core.ttl").unwrap();
            assert_eq!(out.bytes, want, "{rev}");
            assert_eq!(out.repr_type.media_type, "application/octet-stream");
        }
    }

    /// The answer is the blob's bytes: not lossily decoded, not line-converted.
    #[test]
    fn show_answers_the_exact_bytes() {
        let repo = History::new("show-bytes");
        let bytes = [0xff, 0xfe, 0x00, b'\r', b'\n', 0x80, b'x'];
        repo.commit("blob.bin", &bytes);
        assert_eq!(repo.show("HEAD", "blob.bin").unwrap().bytes, bytes);
    }

    /// What ikigai-name's `repo_history` relies on: a path absent at the commit,
    /// though it exists elsewhere in history, is `NotFound`, and so is a rev that
    /// names no commit. Each refusal names the argument and quotes the value.
    #[test]
    fn show_refuses_a_path_or_rev_that_does_not_exist() {
        let repo = History::new("show-absent");
        let before = repo.git(&["rev-parse", "HEAD"]);
        let added = repo.commit("vocab/core.ttl", b"here\n");
        repo.git(&["rm", "-q", "--", "vocab/core.ttl"]);
        repo.git(&["commit", "-q", "-m", "removed"]);
        for rev in [before.as_str(), "HEAD"] {
            let err = repo.show(rev, "vocab/core.ttl").unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{rev}: {err:?}");
            let message = err.to_string();
            assert!(
                message.contains("path") && message.contains("`vocab/core.ttl`"),
                "{message}"
            );
        }
        assert!(repo.show(&added, "vocab/core.ttl").is_ok());

        let zeros = "0".repeat(40);
        for rev in [
            "no-such-branch",
            zeros.as_str(),
            "HEAD..HEAD",
            // These resolve to objects, but not to commits.
            "HEAD^{tree}",
            &format!("{added}:vocab/core.ttl"),
        ] {
            let err = repo.show(rev, "vocab/core.ttl").unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{rev}: {err:?}");
            let message = err.to_string();
            assert!(
                message.contains("rev") && message.contains(&format!("`{rev}`")),
                "{message}"
            );
        }
    }

    /// Only a file is a file: a directory or a submodule at the path is not one.
    /// A symlink answers its stored target and is never followed out of the repo.
    #[test]
    fn show_reads_only_what_the_commit_stores_as_a_file() {
        let repo = History::new("show-kinds");
        repo.commit("vocab/core.ttl", b"here\n");
        let err = repo.show("HEAD", "vocab").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");
        let err = repo.show("HEAD", "vocab/").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");

        let head = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},sub"),
        ]);
        repo.git(&["commit", "-q", "-m", "submodule"]);
        let err = repo.show("HEAD", "sub").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "{err:?}");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/passwd", repo.repo.join("link")).unwrap();
            repo.git(&["add", "--", "link"]);
            repo.git(&["commit", "-q", "-m", "link"]);
            assert_eq!(repo.show("HEAD", "link").unwrap().bytes, b"/etc/passwd");
        }
    }

    /// A path is a name in the commit's tree, relative to the repository's root,
    /// whichever directory the call runs in, and never a pattern.
    #[test]
    fn show_takes_a_path_literally_from_the_repository_root() {
        let repo = History::new("show-literal");
        repo.commit("vocab/core.ttl", b"core\n");
        repo.commit("we:ird*[name]", b"weird\n");
        assert_eq!(
            repo.show("HEAD", "we:ird*[name]").unwrap().bytes,
            b"weird\n"
        );
        for path in [
            "vocab/*.ttl",
            "we*",
            ":(top)vocab/core.ttl",
            ":/vocab/core.ttl",
            "vocab//core.ttl",
            "core.ttl",
        ] {
            let err = repo.show("HEAD", path).unwrap_err();
            assert!(matches!(err, Error::NotFound(_)), "{path}: {err:?}");
        }
        let subdir = repo.repo.join("vocab");
        let out = source_under(
            &[repo.root.as_path()],
            "urn:repo:show",
            &[
                ("dir", subdir.to_str().unwrap()),
                ("rev", "HEAD"),
                ("path", "vocab/core.ttl"),
            ],
            &git_cap(),
        )
        .unwrap();
        assert_eq!(out.bytes, b"core\n");
    }

    /// Nothing that could be read as a flag or leave the tree reaches git, and
    /// `git show --output=` in particular would write a file. Refused by name.
    #[test]
    fn show_refuses_flags_and_paths_out_of_the_tree() {
        let repo = History::new("show-refuse");
        repo.commit("vocab/core.ttl", b"core\n");
        let written = repo.repo.join("written");
        let output = format!("--output={}", written.display());
        for rev in ["", "-x", "--all", output.as_str(), "HE\0AD"] {
            let err = repo.show(rev, "vocab/core.ttl").unwrap_err();
            let Error::InvalidArgument { name, .. } = &err else {
                panic!("{rev:?}: {err:?}");
            };
            assert_eq!(name, "rev", "{rev:?}");
        }
        for path in [
            "",
            "-x",
            "/etc/passwd",
            "../secrets",
            "vocab/../../secrets",
            "./vocab/core.ttl",
            "vocab/./core.ttl",
            "vo\0cab",
        ] {
            let err = repo.show("HEAD", path).unwrap_err();
            let Error::InvalidArgument { name, .. } = &err else {
                panic!("{path:?}: {err:?}");
            };
            assert_eq!(name, "path", "{path:?}");
        }
        assert!(!written.exists(), "a rev must never reach git as a flag");
    }

    #[test]
    fn show_requires_its_arguments_and_the_git_grant() {
        let repo = History::new("show-gate");
        repo.commit("vocab/core.ttl", b"core\n");
        let roots = [repo.root.as_path()];
        for (args, missing) in [
            (vec![("dir", repo.dir()), ("path", "vocab/core.ttl")], "rev"),
            (vec![("dir", repo.dir()), ("rev", "HEAD")], "path"),
        ] {
            let err = source_under(&roots, "urn:repo:show", &args, &git_cap()).unwrap_err();
            match err {
                Error::MissingArgument(name) => assert!(name.starts_with(missing), "{name}"),
                err => panic!("{missing}: {err:?}"),
            }
        }
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        assert_denied(repo.show_as("HEAD", "vocab/core.ttl", &gh), "no git grant");

        let outside = Base::new("show-outside");
        let held = outside.repo("repo");
        assert_denied(
            source_under(
                &roots,
                "urn:repo:show",
                &[
                    ("dir", held.to_str().unwrap()),
                    ("rev", "HEAD"),
                    ("path", "vocab/core.ttl"),
                ],
                &git_cap(),
            ),
            "a dir outside every root",
        );
    }

    /// A broken place to read is not a missing file: git's own failure reaches the
    /// caller rather than reading as "it did not exist then".
    #[test]
    fn show_outside_a_repository_is_a_tool_failure_not_a_miss() {
        let base = Base::new("show-norepo");
        let root = base.dir("root");
        let err = source_under(
            &[&root],
            "urn:repo:show",
            &[
                ("dir", root.to_str().unwrap()),
                ("rev", "HEAD"),
                ("path", "x"),
            ],
            &git_cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Endpoint(_)), "{err:?}");
        assert!(err.to_string().contains("not a git repository"), "{err}");
    }

    /// The same bound as every collected answer: refused whole past the limit.
    #[test]
    fn show_refuses_a_file_over_the_output_limit() {
        let repo = History::new("show-budget");
        repo.commit("big", &vec![b'x'; MAX_OUTPUT_BYTES + 1]);
        repo.commit("fits", &vec![b'x'; MAX_OUTPUT_BYTES]);
        assert_eq!(
            repo.show("HEAD", "fits").unwrap().bytes.len(),
            MAX_OUTPUT_BYTES
        );
        let err = repo.show("HEAD", "big").unwrap_err();
        assert!(matches!(err, Error::OverBudget { .. }), "{err:?}");
    }

    /// In a root, the facades run: the root itself and a repository below it.
    #[test]
    fn a_dir_under_a_root_runs() {
        let base = Base::new("in-root");
        let root = base.dir("root");
        let repo = base.repo("root/repo");
        let held = base.repo("held");
        for iri in GIT_IRIS {
            let out = source_under(
                &[&root],
                iri,
                &[("dir", repo.to_str().unwrap())],
                &git_cap(),
            );
            assert!(out.is_ok(), "{iri}: {out:?}");
            // A root that is itself a repository is in its own jail.
            let out = source_under(
                &[&held],
                iri,
                &[("dir", held.to_str().unwrap())],
                &git_cap(),
            );
            assert!(out.is_ok(), "{iri}: {out:?}");
        }
        let out = source_under(
            &[&root],
            "urn:repo:status",
            &[("dir", repo.to_str().unwrap())],
            &git_cap(),
        )
        .unwrap();
        assert!(
            String::from_utf8_lossy(&out.bytes).starts_with("## "),
            "{out:?}"
        );
    }

    /// A real repository outside every root is refused, typed, by every facade
    /// that takes `dir=`: git would answer there, so an answer would be a spawn.
    /// The gh facades refuse before gh is spawned, so this reaches no network.
    #[test]
    fn a_dir_outside_every_root_is_refused_before_spawning() {
        let base = Base::new("out-root");
        let root = base.dir("root");
        let outside = base.repo("outside");
        let dir = outside.to_str().unwrap();
        for iri in GIT_IRIS {
            assert_denied(
                source_under(&[&root], iri, &[("dir", dir)], &git_cap()),
                iri,
            );
        }
        let gh = Capability::scoped(["urn:cap:exec:gh"]);
        for iri in [
            "urn:repo:pr:list",
            "urn:repo:pr:view",
            "urn:repo:pr:files",
            "urn:repo:pr:diff",
            "urn:repo:pr:checks",
        ] {
            assert_denied(
                source_under(&[&root], iri, &[("dir", dir), ("pr", "1")], &gh),
                iri,
            );
        }
        // Prefixes compare by component: `root-2` is not under `root`.
        let sibling = base.repo("root-2");
        assert_denied(
            source_under(
                &[&root],
                "urn:repo:status",
                &[("dir", sibling.to_str().unwrap())],
                &git_cap(),
            ),
            "a sibling sharing the root's name as a string prefix",
        );
        // An absent directory is refused the same way, so the refusal does not
        // say whether an outside path exists.
        let absent = root.join("absent");
        assert_denied(
            source_under(
                &[&root],
                "urn:repo:status",
                &[("dir", absent.to_str().unwrap())],
                &git_cap(),
            ),
            "an absent directory",
        );
    }

    /// A `..` that climbs out of the root is resolved before the comparison.
    #[test]
    fn a_dotdot_escape_is_refused() {
        let base = Base::new("dotdot");
        let root = base.dir("root");
        base.repo("outside");
        base.dir("root/sub");
        for escape in ["root/../outside", "root/sub/../../outside"] {
            let dir = base.0.join(escape);
            assert_denied(
                source_under(
                    &[&root],
                    "urn:repo:status",
                    &[("dir", dir.to_str().unwrap())],
                    &git_cap(),
                ),
                escape,
            );
        }
    }

    /// A symlink inside the root that points out of it is resolved first; a
    /// root named through a symlink still holds what it points at.
    #[cfg(unix)]
    #[test]
    fn a_symlink_escape_is_refused() {
        let base = Base::new("symlink");
        let root = base.dir("root");
        let outside = base.repo("outside");
        let link = root.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_denied(
            source_under(
                &[&root],
                "urn:repo:status",
                &[("dir", link.to_str().unwrap())],
                &git_cap(),
            ),
            "a symlink out of the root",
        );

        let repo = base.repo("root/repo");
        let root_link = base.0.join("root-link");
        std::os::unix::fs::symlink(&root, &root_link).unwrap();
        let out = source_under(
            &[&root_link],
            "urn:repo:status",
            &[("dir", repo.to_str().unwrap())],
            &git_cap(),
        );
        assert!(out.is_ok(), "{out:?}");
    }

    /// No roots configured: every `dir=` is refused, and the refusal names how
    /// a host configures them. A call without `dir=` is the host's own cwd and
    /// still runs (see `a_facade_declares_its_capability_and_reads_this_repo`).
    #[test]
    fn no_roots_refuses_every_dir() {
        let base = Base::new("no-roots");
        let repo = base.repo("repo");
        let dir = repo.to_str().unwrap();
        for iri in GIT_IRIS {
            let err = source(iri, &[("dir", dir)], &git_cap()).unwrap_err();
            assert!(matches!(err, Error::Denied(_)), "{iri}: {err:?}");
            assert!(
                format!("{err}").contains("space_with_roots"),
                "{iri}: {err}"
            );
            let none: [&std::path::Path; 0] = [];
            assert_denied(source_under(&none, iri, &[("dir", dir)], &git_cap()), iri);
        }
    }

    /// git's upward search for a repository stops at the root: a plain
    /// directory configured as a root, inside a repository, is not a repository.
    #[test]
    fn git_does_not_search_above_the_root() {
        let base = Base::new("ceiling");
        base.repo("outer");
        let root = base.dir("outer/inner");
        // Without the ceiling, git would find `outer` from `inner`.
        let probe = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap();
        assert!(probe.status.success(), "{probe:?}");
        let err = source_under(
            &[&root],
            "urn:repo:status",
            &[("dir", root.to_str().unwrap())],
            &git_cap(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Endpoint(_)), "{err:?}");
        assert!(format!("{err}").contains("not a git repository"), "{err}");
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
    fn list_refuses_root_outside_host_roots() {
        let base = Base::new("list-outside");
        let allowed = base.dir("allowed");
        let outside = base.dir("outside");
        base.dir("outside/secret-repo/.git");
        let fs = Capability::scoped([FS_READ]);
        let result = source_under(
            &[&allowed],
            "urn:repo:list",
            &[("root", outside.to_str().unwrap())],
            &fs,
        );
        if let Ok(out) = &result {
            assert!(String::from_utf8_lossy(&out.bytes).contains("secret-repo\t"));
        }
        assert_denied(result, "list outside host roots");
    }

    #[test]
    fn list_confines_explicit_roots_after_resolving_paths() {
        let base = Base::new("list-boundary");
        let allowed = base.dir("allowed");
        let outside = base.dir("allowed-sibling");
        base.dir("allowed-sibling/hidden/.git");
        let nested = base.dir("allowed/nested");
        base.dir("allowed/nested/visible/.git");
        let fs = Capability::scoped([FS_READ]);
        for path in [
            outside.clone(),
            allowed.join("../allowed-sibling"),
            allowed.join("absent"),
        ] {
            let err = source_under(
                &[&allowed],
                "urn:repo:list",
                &[("root", path.to_str().unwrap())],
                &fs,
            )
            .unwrap_err();
            assert!(matches!(err, Error::Denied(_)), "{err:?}");
            assert!(err.to_string().contains("root=`"), "{err:?}");
        }
        assert_denied(
            source("urn:repo:list", &[("root", allowed.to_str().unwrap())], &fs),
            "explicit root with no configured roots",
        );
        let out = source_under(
            &[&allowed],
            "urn:repo:list",
            &[("root", nested.to_str().unwrap())],
            &fs,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&out.bytes).starts_with("visible\t"));
        #[cfg(unix)]
        {
            let link = allowed.join("escape");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert_denied(
                source_under(
                    &[&allowed],
                    "urn:repo:list",
                    &[("root", link.to_str().unwrap())],
                    &fs,
                ),
                "symlink outside roots",
            );
        }
    }

    #[test]
    fn list_preserves_host_selected_defaults() {
        // Environment overrides belong to child processes, leaving parallel tests untouched.
        const MARKER: &str = "IKIGAI_REPO_LIST_DEFAULT_TEST";
        if std::env::var_os(MARKER).is_some() {
            let out = source("urn:repo:list", &[], &Capability::scoped([FS_READ])).unwrap();
            assert!(String::from_utf8_lossy(&out.bytes).starts_with("default-repo\t"));
            return;
        }
        let base = Base::new("list-default");
        let configured = base.dir("configured");
        base.dir("configured/default-repo/.git");
        base.dir("git-personal/default-repo/.git");
        for explicit_env in [true, false] {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "tests::list_preserves_host_selected_defaults",
                    "--nocapture",
                ])
                .env(MARKER, "1")
                .env("HOME", &base.0)
                .env_remove("IKIGAI_REPO_ROOT");
            if explicit_env {
                child.env("IKIGAI_REPO_ROOT", &configured);
            }
            let out = child.output().unwrap();
            assert!(out.status.success(), "{out:?}");
        }
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
        let out = source_under(
            &[&base],
            "urn:repo:list",
            &[("root", base.to_str().unwrap())],
            &fs,
        )
        .unwrap();
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
        let d = pr_list(no_roots()).describe();
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
            no_roots(),
        )
        .describe();
        let face = d.inputs.iter().find(|a| a.name == "as").unwrap();
        assert!(face.summary.contains("headRefOid"), "{face:?}");
        assert!(d.outputs.iter().any(|o| o == "application/json"), "{d:?}");

        // pr:diff: text-only (no as= arg, one output), pr= required.
        let d =
            gh_pr_facade("repo-pr-diff", "t", "s", &["pr", "diff"], None, no_roots()).describe();
        assert!(d.inputs.iter().all(|a| a.name != "as"), "{d:?}");
        assert_eq!(d.outputs, vec!["text/plain;charset=utf-8".to_string()]);
        assert!(d.inputs.iter().find(|a| a.name == "pr").unwrap().required);

        // log: exec:git, limit default 20, both faces, path= optional.
        let d = log(no_roots()).describe();
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
        let d = pr_files(no_roots()).describe();
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
    fn git_facades_request_plain_output_despite_repository_color() {
        let base = Base::new("plain-output");
        let repo = base.repo("repo");
        let out = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "color.ui", "always"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        std::fs::write(repo.join("untracked"), "content").unwrap();
        for iri in GIT_IRIS {
            let out = source_under(
                &[&base.0],
                iri,
                &[("dir", repo.to_str().unwrap())],
                &git_cap(),
            )
            .unwrap();
            assert!(!out.bytes.is_empty(), "{iri}: empty answer");
            assert!(
                !out.bytes.contains(&0x1b),
                "{iri}: {:?}",
                String::from_utf8_lossy(&out.bytes)
            );
        }
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
        let out = source_under(
            &[&base],
            "urn:repo:log",
            &[("limit", "2"), ("dir", dir)],
            &git,
        )
        .unwrap();
        assert!(out.repr_type.media_type.starts_with("text/plain"));
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 2, "{body:?}");
        assert!(body.lines().next().unwrap().contains("third"), "{body:?}");

        // JSON face: [{hash, author, date, subject}], full sha, RFC 3339 date.
        let out = source_under(
            &[&base],
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
        let err = source_under(&[&base], "urn:repo:log", &[("limit", "1; rm")], &git).unwrap_err();
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
        let out = source_under(
            &[&base],
            "urn:repo:log",
            &[("dir", dir), ("path", "sub")],
            &git,
        )
        .unwrap();
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 1, "{body:?}");
        assert!(body.contains("touch sub (#2)"), "{body:?}");

        // …and composes with limit=: two a-touching commits, newest first,
        // limit=1 keeps only the newest.
        let out = source_under(
            &[&base],
            "urn:repo:log",
            &[("dir", dir), ("path", "a.txt"), ("limit", "1")],
            &git,
        )
        .unwrap();
        let body = String::from_utf8_lossy(&out.bytes).into_owned();
        assert_eq!(body.lines().count(), 1, "{body:?}");
        assert!(body.contains("touch a again (#3)"), "{body:?}");

        // The JSON face takes the same pathspec: honest subjects, same scope.
        let out = source_under(
            &[&base],
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
        let err = source_under(
            &[&base],
            "urn:repo:log",
            &[("dir", dir), ("path", "--all")],
            &git,
        )
        .unwrap_err();
        assert!(
            format!("{err:?}").contains("path must be a pathspec"),
            "{err:?}"
        );
        // The empty pathspec is refused with our error, not git's fatal.
        let err = source_under(
            &[&base],
            "urn:repo:log",
            &[("dir", dir), ("path", "")],
            &git,
        )
        .unwrap_err();
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
