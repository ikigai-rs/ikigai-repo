# Changelog

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
