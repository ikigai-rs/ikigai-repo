# ikigai-repo

Git and `gh` as [ikigai](https://github.com/ikigai-rs) resources — the
**platform seam** for development tooling.

Typed, capability-gated **facades**, each of which builds a fixed argument
vector for `git` or `gh`; a caller supplies only typed values (a number, a state
from a closed vocabulary, a pathspec, a directory, an `owner/name`), never the
argv. There is **no low-level exec endpoint** (see [Changes](#changes)):

| resource | what it is |
|----------|------------|
| `urn:repo:status` | the working tree's status (`git status --porcelain=v1 -b`) |
| `urn:repo:log` | recent history (`git log --oneline`, `limit=` commits, default 20); `path=` restricts to commits touching a file/subtree (`git log -- <path>`) — under squash-merge style, the merged-PR-per-path index; `as=application/json` → `[{hash, author, date, subject}]` (full sha, RFC 3339 date) |
| `urn:repo:branch` | the current branch |
| `urn:repo:list` | enumerate the git repos under a ROOT (`root=`, else `$IKIGAI_REPO_ROOT`, else `~/git-personal`) as `name`⇥`path`; a filesystem read, gated on `urn:cap:fs:read:*` (no exec cap) |
| `urn:repo:pr:list` | the PRs, most recently updated first (`gh pr list`), one per line as `number`⇥`title`⇥`state`⇥`branch`⇥`updated` (RFC 3339); `state=` (open/closed/merged/all, default open); `as=application/json` for the structured face; `limit=` (default 30) |
| `urn:repo:pr:view` | a PR's title, state, and metadata (`gh pr view N`); `as=application/json` for the structured face, incl. `headRefOid` — the PR head commit sha |
| `urn:repo:pr:files` | the paths a PR changes (`gh pr view N --json files`), one per line; `as=application/json` → `{files: [{path, additions, deletions, changeType}]}` — the open-PR half of "which PRs touch this subtree" (`urn:repo:log path=` is the merged half) |
| `urn:repo:pr:diff` | a PR's unified diff (`gh pr diff N`) — the raw change an explainer/reviewer reads |
| `urn:repo:pr:checks` | the CI check runs for a PR and their state (`gh pr checks N`) — a snapshot, not a wait |

The `pr:*` facades take `pr=` (the number) plus optional `repo=` (`owner/name`)
or `dir=` (a repo directory to run in); they require `urn:cap:exec:gh`.

Each facade declares and enforces the concrete capability it needs:
`urn:cap:exec:git` for `status`/`log`/`branch`, `urn:cap:exec:gh` for the
`pr:*` family. Those grants gate only these facades: holding
`urn:cap:exec:git` does not let a caller choose what git runs.

Every git/gh invocation accepts at most **8 MiB (8,388,608 raw bytes) per
output pipe**, stdout and stderr (16 MiB combined). At exactly the limit the
output is accepted; one byte beyond either limit kills and reaps the subprocess
and refuses the entire call with typed `OverBudget`. Output is never truncated
or returned partially, including on a nonzero exit. Large diffs or logs must be
narrowed before retrying.

A facade pointed with `dir=` at a repository honors that repository's own
configuration, as git and gh always do; `dir=` is not confined to a root.

**The verb tells the truth**: a read (`git status`) is a `Source`; a mutation
(`git commit`) is a `Sink` under a write capability. v1 ships the read facades.

**Native-only by nature** — it spawns subprocesses, the one thing a wasm module
can't. It is to the shell what `ikigai-personal` is to EventKit.

```rust
use ikigai_core::Kernel;
use std::sync::Arc;
let kernel = Kernel::new(Arc::new(ikigai_repo::space()));
// source urn:repo:status dir=/path/to/repo   (under a urn:cap:exec:git grant)
```

Run `cargo run --example repo-demo` to watch it read its own git state.

The space names itself `urn:iki:space:repo` (`ikigai_repo::SPACE_ID`): building it reads nothing.

## Conformance

The module **passes
[`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance)**
(`tests/conformance.rs`): every check runs — typed inputs, declared = enforced,
cacheability, pipeline citizenship, naming — over a hermetic scratch repository
(CI checkouts are shallow, so nothing reads this repo's own history). Nothing
here is cached, by design: every read is of a working tree, git state, a
directory, or GitHub, and no host signal names "this working tree changed" —
the workspace watcher cuts `urn:file:<path>` per file, relative to a root the
module cannot see from an absolute `dir=` — so every representation is live.
The `urn:repo:pr:*` facades are opted out of the checks that fire them
("reaches api.github.com"); the static checks still hold them, and their
capability gate is pinned by a test of its own.

## Changes

**Unreleased (rides the next release):** `urn:system:exec` is no longer served
(ledger #1706). It took a caller's argument vector (`tool=` + `args=` + `dir=`)
gated only by a tool allowlist and `urn:cap:exec:{tool}`, and an argv is not a
safe boundary for these tools: git, gh, cargo and just each have flags and
configuration that run other programs, so a per-tool grant was a grant to run
anything. The space now answers that name `Unresolved`.

- **Consumers lose the `system-exec` tool at their next bump of this crate**:
  gonk, and the cli's MCP projection (which projects every endpoint the space
  binds), stop offering it. Nothing outside this crate called it.
- **The per-tool exec grants (`urn:cap:exec:git`, `urn:cap:exec:gh`,
  `urn:cap:exec:cargo`) now gate only the typed `urn:repo:*` facades.** No facade
  runs cargo or just, so `urn:cap:exec:cargo` gates nothing here.
- The facades are unchanged: same names, arguments, faces and capabilities.

## Accessibility

What this crate writes that people read (its own text faces and refusals), which output is
git's and gh's and passes through unchanged, the rules it follows and its known limitations are
in [ACCESSIBILITY.md](ACCESSIBILITY.md). Report a barrier with an issue labeled
`accessibility`.

## License
Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
