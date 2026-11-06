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
| `urn:repo:show` | one file's bytes at a commit (`git cat-file`): `rev=` (a commit hash, branch or tag) and `path=` (relative to the repository root, taken literally) are required; answers `application/octet-stream`, or typed `NotFound` when `rev=` names no commit or `path=` is not a file at that commit |
| `urn:repo:{repo}:object:sha:{sha}` | a locally held Git object by full SHA (40 or 64 hexadecimal digits): its `sha`, `type` (`commit`, `tree`, `blob`, `tag`), and for a blob its first commit and path; text by default, `as=application/json` for the structured face; requires `urn:cap:exec:git` |
| `urn:repo:branch` | the current branch |
| `urn:repo:list` | enumerate the git repos under a ROOT (`root=`, else `$IKIGAI_REPO_ROOT`, else `~/git-personal`) as `name`⇥`path`; a filesystem read, gated on `urn:cap:fs:read:*`; explicit `root=` must resolve under a host-configured repository root (no exec cap) |
| `urn:repo:pr:list` | the PRs, most recently updated first (`gh pr list`), one per line as `number`⇥`title`⇥`state`⇥`branch`⇥`updated` (RFC 3339); `state=` (open/closed/merged/all, default open); `as=application/json` for the structured face; `limit=` (default 30) |
| `urn:repo:pr:view` | a PR's title, state, and metadata (`gh pr view N`); `as=application/json` for the structured face, incl. `headRefOid` — the PR head commit sha |
| `urn:repo:pr:files` | the paths a PR changes (`gh pr view N --json files`), one per line; `as=application/json` → `{files: [{path, additions, deletions, changeType}]}` — the open-PR half of "which PRs touch this subtree" (`urn:repo:log path=` is the merged half) |
| `urn:repo:pr:diff` | a PR's unified diff (`gh pr diff N`) — the raw change an explainer/reviewer reads |
| `urn:repo:pr:checks` | the CI check runs for a PR and their state (`gh pr checks N`) — a snapshot, not a wait |

The `pr:*` facades take `pr=` (the number) plus optional `repo=` (`owner/name`)
or `dir=` (a repo directory to run in); they require `urn:cap:exec:gh`.

Each facade declares and enforces the concrete capability it needs:
`urn:cap:exec:git` for `status`/`log`/`show`/`branch`/`object`, `urn:cap:exec:gh` for the
`pr:*` family. Those grants gate only these facades: holding
`urn:cap:exec:git` does not let a caller choose what git runs.

The object resource uses the repository directory's name for `{repo}`. For example,
`urn:repo:ikigai-repo:object:sha:<full-sha>` with `dir=/served/ikigai-repo`
looks in that repository. `dir=` follows the same configured-root confinement as
other git reads; omitting it uses the process cwd. A name that does not match the
repository directory is `InvalidArgument` for `repo`. A malformed SHA is
`InvalidArgument` for `sha`, naming the required 40- or 64-digit hexadecimal form.
Uppercase hexadecimal digits are accepted and normalized to lowercase.

The JSON shape is `{"sha":"…","type":"blob","firstAppearance":{"commit":"…","path":"…"}}`.
`firstAppearance` is `null` for other object types and for a blob with no known
appearance. Blob provenance uses `git log --all --reverse --topo-order --root -m
--find-object=<sha>`, then checks each candidate tree for the blob. It includes all
local refs and merge introductions, skips removals, and returns the first matching
path in Git's tree order. Independent histories follow Git's traversal order.
Only retained reachable history is available: shallow clones, removed refs and
unreferenced blobs can limit the answer. Paths are literal repository-relative
UTF-8 strings; a non-UTF-8 path is refused rather than changed. The text face names
each field and quotes the path as a JSON string, escaping control characters.
Replacement refs are disabled for these reads, and missing promisor objects are
not fetched.

Present object answers have no time expiry (`Expiry::Never`). They carry the
`urn:repo:{repo}:object-history` thread: a host can cut it after fetching history,
rewriting refs or pruning objects to refresh local provenance. This crate does
not watch repositories or cut that thread itself. An absent SHA returns
`NotFound` with a stated five-second upper bound; core 0.3 does not cache
`NotFound` at all, so a later lookup can see a newly fetched object immediately.
Tool failures remain tool failures rather than absence.

Every git/gh invocation accepts at most **8 MiB (8,388,608 raw bytes) per
output pipe**, stdout and stderr (16 MiB combined). At exactly the limit the
output is accepted; one byte beyond either limit kills and reaps the subprocess
and refuses the entire call with typed `OverBudget`. Output is never truncated
or returned partially, including on a nonzero exit. Large diffs or logs must be
narrowed before retrying.

A facade pointed with `dir=` at a repository honors that repository's own
configuration, as git and gh always do; `dir=` must resolve under a root supplied
by the host with `space_with_roots`. The default `space()` refuses explicit
directories because it has no configured roots.

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
(CI checkouts are shallow, so nothing reads this repo's own history). The object
resource is declared cacheable and checked for cache hits, identical recomputation
after thread cuts, and agreement between collected and body answers. Other reads
remain live: working-tree state, directories and GitHub change independently of
these calls.
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
