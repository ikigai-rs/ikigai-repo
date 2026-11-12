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
| Last reviewed | 2026-11-12 |

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
  not a valid value, a `repo` that is not `owner/name` (``repo `…` is not owner/name: give an
  owner and a repository name``) (`state must be one of open, closed, merged, all, got …`), a `path` that is
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

Every named check has a result: Pass, Fail or Blocked, with evidence. A Pass covers only
that check, not overall accessibility or WCAG conformance. Blocked means the check could not
be completed; it is not a Pass. No historical run is counted as current verification.

The automated checks below ran on 2026-11-12 against unchanged main commit
[`ee622ee`](https://github.com/ikigai-rs/ikigai-repo/commit/ee622eea010b94e316d7ab10d5ed017a8b5a8948),
on macOS 27.2, with rustc 1.99.1, Cargo 1.99.1 and Git 2.54.0 (Apple Git-157).
The command was `cargo test --workspace -- --skip non_utf8_object_path_is_refused_and_status_uses_git_quoting`:
58 unit tests, 7 audit tests, 4 module-contract tests, 1 host-environment test, 3 stream tests
and 1 doctest passed. The excluded check is recorded separately below. CI's
[workflow](.github/workflows/ci.yml) calls the shared Rust workflow, which runs
`cargo test --workspace` without this exclusion. These tests inspect bytes, arguments,
metadata and typed errors; they do not evaluate how a host presents them to a person.

| Check | Result | Evidence |
|---|---|---|
| JSON log content and repository-root pathspecs | Pass | `tests/audit.rs`, run by the command above: collected and streamed JSON preserve a unit separator in author names and subjects, empty subjects, and long multiline subjects with control characters. Pathspecs retain their meaning from a subdirectory. This checks data preservation, not spoken or visual presentation. |
| Argument, capability and directory refusals | Pass | `src/lib.rs`: `pr_repo_must_be_owner_and_name`, `pr_list_and_diff_are_gated`, `run_is_capability_gated_and_allowlisted`, `no_roots_refuses_every_dir`, `list_confines_explicit_roots_after_resolving_paths`, `git_does_not_search_above_the_root`, `show_refuses_a_path_or_rev_that_does_not_exist` and `show_refuses_flags_and_paths_out_of_the_tree`, run by the command above. Pin argument names, quoted values, closed vocabularies and selected tool errors; do not establish clarity of every refusal. |
| Text-face templates and facade descriptions | Pass | `src/lib.rs`: `pr_list_builds_the_gh_invocation`, `pr_files_builds_the_gh_invocation` and `descriptions_declare_faces_args_and_caps`, run by the command above. Pin templates and declared faces, arguments and capabilities; do not test a host's help or catalog rendering. |
| Object labels, escaped paths, typed refusals and cache behavior | Pass | `src/lib.rs`: `object_types_and_faces_in_a_scratch_repository`, `object_refuses_malformed_sha_and_wrong_repository`, `object_is_gated_in_the_kernel_and_endpoint` and `object_absence_expires_and_presence_is_cached`, run by the command above. Pin labeled text, JSON-quoted control characters, JSON fields for four object types, SHA form, capability refusals and cache expiry; do not test assistive technology. |
| Output-limit refusals and streamed tool failure | Pass | `src/lib.rs`: `output_reader_accepts_exact_limit_and_refuses_next_byte`; `src/process_body.rs`: `exact_budget_succeeds_next_byte_is_error_item` and `late_nonzero_exit_is_an_error_item_with_stderr`, run by the command above. Pin the collected limit message, streamed budget error, exit status and stderr; do not establish that every error gives enough recovery guidance. |
| Plain git output under forced repository color | Pass | `src/lib.rs`: `git_facades_request_plain_output_despite_repository_color`; `tests/streams.rs`: `log_body_is_incremental_and_matches_bytes_in_both_faces`, run by the command above. Check status, branch and log for ESC bytes with `color.ui=always`, and agreement between collected and streamed log faces. Do not strip or evaluate arbitrary repository content. |
| Plain-output environment for collected and streamed gh diff | Pass | `tests/streams.rs`: `pr_diff_body_matches_bytes_without_reaching_github`, run by the command above. A local shell fixture checks `NO_COLOR=1`, `CLICOLOR=0`, `CLICOLOR_FORCE=0` and absence of `GH_FORCE_TTY` while the parent forces color and terminal output. It does not execute real gh, contact GitHub or evaluate gh's rendering. |
| Module contracts, host environment and API example | Pass | `tests/conformance.rs`, `tests/host_env.rs` and the `space_with_roots` doctest, run by the command above. Check module metadata, typed grants and required arguments, local media types, host-environment isolation and example compilation. The module-contract suite opts out of live GitHub calls; none of these checks establish WCAG conformance. |
| Non-UTF-8 object-path refusal and git status quoting | Blocked | `cargo test --all-targets` on the same main commit and tools failed in `tests/audit.rs`: `non_utf8_object_path_is_refused_and_status_uses_git_quoting` could not create its filename fixture on macOS (`Illegal byte sequence`, OS error 92). It did not reach the crate's refusal or quoting assertions. Other unusual-path audit fixtures passed; they do not cover this case. |
| Automated accessibility checker | Blocked | No axe or other accessibility checker was run on a host presenting this output; no host page or checker session was configured for this evaluation. Rust tests are not an accessibility checker. |
| Browser presentation, zoom, reflow and contrast | Blocked | No browser evaluation was run on a host presenting this output. The crate emits no HTML or SVG; host presentation still needs evaluation. |
| Manual keyboard | Blocked | No person performed a keyboard pass in a terminal or host presenting this output; no manual session was available for this evaluation. |
| Screen reader | Blocked | No screen reader was used on this output; no screen reader session was available for this evaluation. |
| Screen magnifier | Blocked | No screen magnifier was used on this output; no magnifier session was available for this evaluation. |
| Speech input | Blocked | No speech-input software was used on this output; no speech-input session was available for this evaluation. |
| Switch access | Blocked | No switch-access software was used on this output; no switch-access session was available for this evaluation. |
| Braille display | Blocked | No braille display was used on this output; no braille display was available for this evaluation. |

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
git's and gh's words unchanged. In the pull request's Accessibility section, give each check
a row in a `Check | Result | Evidence` table: Pass, Fail or Blocked, with the test or command,
tool and version used, or why it could not run. Keep unrun checks as Blocked, including every
assistive technology nobody used. A historical run CI does not repeat stays Blocked with its
date. State what each Pass covers and does not cover.

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
| 2026-11-12 | Record check results, current main evidence and blocked host and assistive-technology evaluations. |
