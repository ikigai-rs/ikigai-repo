# ikigai-repo

Git, `gh`, and `cargo` as [ikigai](https://github.com/ikigai-rs) resources — the
**platform seam** for development tooling.

A capability-gated **exec** endpoint runs an *allowlisted* tool with an
**argument vector** (never a shell string, so there is no injection surface),
and typed **facades** sit over it:

| resource | what it is |
|----------|------------|
| `urn:system:exec` | run an allowlisted tool (`git`/`gh`/`cargo`/`just`) with `tool=` + `args=` (one arg per line) + `dir=`; gated on `urn:cap:exec:{tool}` |
| `urn:repo:status` | the working tree's status (`git status --porcelain=v1 -b`) |
| `urn:repo:log` | recent history (`git log --oneline -20`) |
| `urn:repo:branch` | the current branch |

Two layers gate every call: the **allowlist** (the outer bound — an unknown tool
is refused before a process is spawned) and the **capability** (the inner bound —
`urn:system:exec` requires `urn:cap:exec:{tool}`; each facade declares the
concrete scope it needs). The manifold offers exec under the wildcard
`urn:cap:exec:*`, so an agent only ever sees it under an exec grant.

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

## License
Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).
