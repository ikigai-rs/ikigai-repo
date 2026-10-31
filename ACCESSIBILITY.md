# Accessibility

What `ikigai-repo` emits that people read, the rules that output follows, how it is tested, and
how to report a barrier. It is a library: it runs `git` and `gh` with fixed arguments and
answers with their output as text, and a host decides how a person sees it. Much of that text is
git's and gh's own; this file says which. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only, no HTML or SVG |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-repo/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-repo/security/advisories/new) |
| Last reviewed | 2026-10-31 |

## What it emits that people read

All of it is in `src/lib.rs` unless named otherwise.

- **git's and gh's own output, passed through**: `urn:repo:status` (`git status
  --porcelain=v1 -b`), `urn:repo:log` (`git log --oneline`), `urn:repo:branch`,
  `urn:repo:pr:view`, `urn:repo:pr:diff` and `urn:repo:pr:checks`. The words, codes and layout
  are git's and gh's.
- **Text faces this crate shapes**: `urn:repo:pr:list` (one PR per line, `number`, `title`,
  `state`, `branch`, `updated` separated by tabs, through a gh template), `urn:repo:pr:files`
  (one path per line) and `urn:repo:list` (`name`, a tab, `path`). The JSON faces
  (`as=application/json`) are for programs.
- **Refusals and errors**: a capability that does not grant `urn:cap:exec:git` or
  `urn:cap:exec:gh`, a `dir=` outside the configured roots, a `limit`, `pr` or `state` that is
  not a valid value (`state must be one of open, closed, merged, all, got …`), a `path` that is
  a flag, output past the 8 MiB limit, and a tool that failed: `` exec: `git` exited N: `` and
  then the tool's own stderr (`src/process_body.rs` does the same for a streamed answer).
- **The facades' self-descriptions**: the summaries of every facade and argument, which a host
  shows in its catalog, help or tool list.
- **The example** (`examples/repo-demo.rs`) prints three answers, or their errors, to a
  terminal, as plain lines.

Not this repository's: the words in git's and gh's output and in their error messages; the
words around an error (`Error`'s `Display`, from `ikigai-core`); and any page, terminal or tool
that shows these.

## The rules that output follows

- A refusal of an argument names the argument and quotes the value it got; a closed vocabulary
  is listed in the refusal.
- A tool that fails is named, with its exit status, and its own stderr is passed on, not
  reworded.
- Output past the limit is never handed over as if complete: a collected answer is refused
  whole, and the refusal states the limit and that no output is returned; a streamed answer
  (`src/process_body.rs`) ends with an error.
- This crate adds no color and no terminal control codes of its own.

## How it is tested

- `src/lib.rs`: `pr_list_and_diff_are_gated` pins `limit must be a number` and the `state`
  refusal's list; `output_reader_accepts_exact_limit_and_refuses_next_byte` that the refusal
  states the limit; `run_is_capability_gated_and_allowlisted` the allowlist refusal;
  `no_roots_refuses_every_dir` that the `dir=` refusal names what to configure;
  `git_does_not_search_above_the_root` that git's own `not a git repository` reaches the
  caller; `pr_list_builds_the_gh_invocation` the text face's template.
- `src/process_body.rs`: `late_nonzero_exit_is_an_error_item_with_stderr` pins the exit status
  and the tool's stderr in a streamed failure.

**No automated accessibility checker, browser or assistive technology has been used on this
output.**

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

- Nothing asks git or gh for plain output (no `--no-color`, no `NO_COLOR`). They write no color
  to a pipe by default, but a repository's git configuration (`color.ui=always`) or a forcing
  variable in the host's environment that gh honors would put terminal color codes into the
  answer, and this crate passes them on.
- `urn:repo:status` answers git's porcelain format, whose meaning is in two-letter codes (` M`,
  `??`) and a `##` branch line, not in words.
- The tab-separated faces (`pr:list`, `list`) have no header line naming their columns.
- `` exec: `git` exited N: … `` names the tool but not the facade or the directory it ran in.
- A streamed answer past the run's byte budget ends with `exec: process body exceeds the run's
  byte budget`, which names neither the tool, the budget nor how to raise it.
- The example prints an error in its debug form (`{e:?}`), not as a sentence.

## Contributing

Read this file before changing any output listed above: a text face, a refusal's words or a
facade's description. Keep meaning in words, name the argument a refusal is about, and keep
git's and gh's words unchanged. Say in the pull request what was tested, with what, and what was
not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-repo/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your terminal and any assistive
technology you use, if you are comfortable sharing them. Anything security-sensitive goes
through
[private vulnerability reporting](https://github.com/ikigai-rs/ikigai-repo/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
