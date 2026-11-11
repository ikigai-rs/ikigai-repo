# Changelog

## 0.5.1

- `urn:repo:log` frames each record with NUL-terminated fields, so an author name containing the
  old separator no longer shifts the date and subject; `path=` is anchored at the repository root
  (a call from a subdirectory returned an empty log) while keeping pathspec magic (ledger #3447).
- Refuse bare repositories git would adopt from `dir=` (`safe.bareRepository=explicit` for git
  and for the git gh runs). Tracked files inside a served repository could spell one, with its
  own `core.fsmonitor` command (run by `urn:repo:status`) or a `commondir` naming a repository
  outside every root (read by `urn:repo:log` and `urn:repo:show`) (ledger #3487).
- Remove `GIT_DIR` and the other repository-selecting variables from every child's
  environment, so an inherited one cannot replace `dir=`; git no longer receives the host's
  GitHub tokens.
- The `pr:*` facades refuse a `repo=` that is not `owner/name` with typed `InvalidArgument`.
  gh read `HOST/OWNER/REPO` and connected to that host.

## 0.5.0

- Add `urn:repo:{repo}:object:sha:{sha}`: whether a repository holds a git object by its full
  SHA (40 or 64 hex digits), its type (commit, tree, blob, tag) and, for a blob, the commit that
  introduced it and its path there. Under `urn:cap:exec:git`; a present answer never expires and
  hangs on the repository's object-history thread; an absent one is `NotFound`. For gonk's links
  from commit and blob IRIs into code (ledger #3204, #3198).
- Add `urn:repo:show` (`dir`, `rev`, `path`): a file's exact bytes at a commit,
  under `urn:cap:exec:git`, confined to the roots like every `dir=`. A rev that
  names no commit and a path that is not a file at that commit answer typed
  `NotFound`; a git failure stays an `Endpoint` error, so it never reads as a
  miss. This is the read `ikigai-name`'s as-of binding (`repo_history`) calls.

## 0.4.0

- Re-pin to `ikigai-core` 0.3.0, the scoped content store (ledger #2965), and
  `ikigai-conformance` 0.14.0 in tests. Core's types are in this crate's public
  API, so this is a minor bump. This crate calls neither `ContentStore::put` nor
  `put_replay`, and no output changes.

## 0.3.2

- Confine an explicit `urn:repo:list` `root=` to the host-configured repository
  roots after resolving symlinks and `..`. Preserve the host-selected default
  (`IKIGAI_REPO_ROOT`, else `~/git-personal`) when `root=` is omitted.

## 0.3.1

- Stream `urn:repo:log` (text and JSON) and `urn:repo:pr:diff` through
  `Endpoint::invoke_body` as one-shot bodies under the run's byte budget.
- Read stdout on demand, retain bounded stderr, and kill and reap the direct
  child when the body is dropped. Overflow and unsuccessful exits end the
  stream with an error item.
- Preserve the bytes `invoke` implementation and its per-pipe 8 MiB bound.
