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
| Last reviewed | 2026-11-06 |

## What it emits that people read

All of it is in `src/lib.rs` unless named otherwise.

- **git's and gh's own output, passed through**: `urn:repo:status` (`git status
  --porcelain=v1 -b`), `urn:repo:log` (`git log --oneline`), `urn:repo:branch`,
  `urn:repo:pr:view`, `urn:repo:pr:diff` and `urn:repo:pr:checks`. The words, codes and layout
  are git's and gh's. `urn:repo:show` passes on a file's bytes from a repository, unchanged and
  as `application/octet-stream`: they may not be text, and they are not this crate's words.
- **Text faces this crate shapes**: `urn:repo:pr:list` (one PR per line, `number`, `title`,
  `state`, `branch`, `updated` separated by tabs, through a gh template), `urn:repo:pr:files`
  (one path per line) and `urn:repo:list` (`name`, a tab, `path`). The JSON faces
  (`as=application/json`) are for programs. The object resource
  (`urn:repo:{repo}:object:sha:{sha}`) labels `SHA`, `Type`, `First commit` and
  `Path (JSON string)` on separate lines. The path is JSON-quoted so tabs,
  newlines and other control characters stay visible. A blob with no known
  introduction says `First appearance: not found in reachable history`. Its JSON
  face uses `sha`, `type` and `firstAppearance` (a commit and path, or `null`).
- **Refusals and errors**: a capability that does not grant `urn:cap:exec:git` or
  `urn:cap:exec:gh`, a `dir=` or explicit list `root=` outside the configured roots, a `limit`, `pr` or `state` that is
  not a valid value (`state must be one of open, closed, merged, all, got …`), a `path` that is
  a flag, a `rev` or `path` that `urn:repo:show` cannot take or that does not exist (``rev `…`
  does not name a commit in this repository``, ``path `…` is not a file at commit …``), output
  past the 8 MiB limit, and a tool that failed: `` exec: `git` exited N: `` and
  then the tool's own stderr (`src/process_body.rs` does the same for a streamed answer).
- **Object lookup refusals**: malformed `sha` values are `InvalidArgument` and name
  the form (`must be a full SHA: 40 or 64 hexadecimal digits`); a mismatched `repo`
  names the repository directory requirement. Missing objects are `NotFound`
  (`sha … is not held in this repository (absence expires after 5 seconds)`).
  These quoted values escape control characters. A non-UTF-8 object path says
  `object path is not UTF-8; no output returned`. Capability and directory refusals,
  tool failures and the 8 MiB per-pipe limit follow the other git reads.
- **The facades' self-descriptions**: the summaries of every facade and argument, which a host
  shows in its catalog, help or tool list.
- **The example** (`examples/repo-demo.rs`) prints three answers, or their errors, to a
  terminal, as plain lines.

Not this repository's: the words in git's and gh's output and in their error messages; the
words around an error (`Error`'s `Display`, from `ikigai-core`); and any page, terminal or tool
that shows these.

## The rules that output follows

- A refusal of an argument names the argument and quotes the value it got; a closed vocabulary
  is listed in the refusal. `urn:repo:show` escapes control characters in the value it quotes.
- A tool that fails is named, with its exit status, and its own stderr is passed on, not
  reworded.
- Output past the limit is never handed over as if complete: a collected answer is refused
  whole, and the refusal states the limit and that no output is returned; a streamed answer
  (`src/process_body.rs`) ends with an error.
- Every tool call requests plain output: git gets `-c color.ui=never`; gh gets
  `NO_COLOR=1`, `CLICOLOR=0`, `CLICOLOR_FORCE=0`, and no `GH_FORCE_TTY` override.
  This applies to collected and streamed answers. It does not strip control bytes from
  repository content, such as file bytes or commit messages.
- This crate adds no color and no terminal control codes of its own.

## How it is tested

- `src/lib.rs`: `pr_list_and_diff_are_gated` pins `limit must be a number` and the `state`
  refusal's list; `output_reader_accepts_exact_limit_and_refuses_next_byte` that the refusal
  states the limit; `run_is_capability_gated_and_allowlisted` the allowlist refusal;
  `no_roots_refuses_every_dir` that the `dir=` refusal names what to configure;
  `list_confines_explicit_roots_after_resolving_paths` that list refusals name `root=`;
  `git_does_not_search_above_the_root` that git's own `not a git repository` reaches the
  caller; `pr_list_builds_the_gh_invocation` the text face's template;
  `show_refuses_a_path_or_rev_that_does_not_exist` that those refusals name the argument and
  quote the value, and `show_refuses_flags_and_paths_out_of_the_tree` that the others name it.
- `src/lib.rs`: `object_types_and_faces_in_a_scratch_repository` pins labeled text,
  escaped control characters in a path, and the JSON fields for all four types;
  `object_refuses_malformed_sha_and_wrong_repository` checks typed refusals and
  the SHA form. `object_is_gated_in_the_kernel_and_endpoint` checks the capability
  refusal in both layers. `object_absence_expires_and_presence_is_cached` checks
  that a miss is not retained and a successful answer has no time expiry.
- `src/process_body.rs`: `late_nonzero_exit_is_an_error_item_with_stderr` pins the exit status
  and the tool's stderr in a streamed failure.

- `src/lib.rs`: `git_facades_request_plain_output_despite_repository_color` checks status,
  branch and log answers for ESC bytes with `color.ui=always` in a scratch repository.
  `tests/streams.rs` checks both log paths and faces with that configuration, and a local
  gh fixture checks the plain-output environment on collected and streamed diff calls
  while the parent environment forces color and terminal output. The fixture does not
  contact GitHub or test gh's own rendering.

**No automated accessibility checker, browser or assistive technology has been used on this
output.**

## Known limitations

Found by reading the source on 2026-10-31, not by testing:

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
| 2026-11-03 | Add `urn:repo:show`: its passed-through bytes and its refusals. |
| 2026-11-06 | Add object lookup: labeled text, JSON-quoted paths, SHA and repository refusals, and scratch-repository tests. |
| 2026-11-06 | Request plain git and gh output; test forced-color configuration and environment. |
