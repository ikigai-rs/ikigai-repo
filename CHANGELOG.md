# Changelog

## 0.3.1

- Stream `urn:repo:log` (text and JSON) and `urn:repo:pr:diff` through
  `Endpoint::invoke_body` as one-shot bodies under the run's byte budget.
- Read stdout on demand, retain bounded stderr, and kill and reap the direct
  child when the body is dropped. Overflow and unsuccessful exits end the
  stream with an error item.
- Preserve the bytes `invoke` implementation and its per-pipe 8 MiB bound.
