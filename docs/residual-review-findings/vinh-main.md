# Residual review findings — `vinh-main`

`vinh-main` is a long-lived branch, so this file accumulates one section per shipped change rather
than being replaced. Newest last.

---

# Run 1 — repo-mode tab reorder

Branch: `vinh-main` (base `0be44f0d9`)
Change: repo-mode tab reorder fix (plan `docs/plans/2026-08-05-001-fix-repo-mode-tab-reorder-plan.md`)
Review run: `20260805-100129-7d46f576`

Reviewers: correctness, adversarial, testing, maintainability, performance, project-standards.
Five findings survived mechanical merge; an independent validator confirmed three and rejected two.
One was applied. This file records everything not applied.

No tickets were filed. This repository's issue tracker belongs to the upstream OSS project, and
filing there for an unmerged personal branch was not authorized.

---

## Not applied — needs a decision

### R1. Non-adjacent section swap relocates a tab the user did not drag

`app/src/workspace/view.rs:29333` — from `adversarial`, P2, confidence 75, validator **confirmed**.

Before this change, `calculate_updated_tab_index` could only ever return `current_index ± 1`, so
`self.tabs.swap(new_index, current_index)` only ever exchanged visually adjacent tabs. Section-scoped
neighbour resolution can now return an index many slots away, and the swap primitive was left
unchanged. With the suite's own `repo_run_fixture` order `[L0, A1, A2, A3, L4]`, dragging `L4` left
past the repository run resolves neighbour `0`, and the swap puts `L0` at index 4 — four slots right,
with no gesture on it.

The validator confirmed the path is reachable: `target_group_at_axis` returns `None` once the cursor
clears the group rect (`view.rs:29649-29662`), the pinned check passes for two unpinned loose tabs,
and the collapsed-group hop at `view.rs:29317-29331` only fires for a collapsed foreign group.

**Why it was not applied.** This is a UX semantics decision, not a defect with one right answer.
Both readings are defensible:

- *Swap* (today): the two members of the loose section exchange places. Every other tab keeps its
  absolute index, and the repository run is untouched. Within the virtual section list, the two tabs
  did reorder correctly.
- *Hop* (`hop_tab_to_index`, the reviewer's proposal): only the dragged tab moves; everything between
  shifts by one. This matches what a drag gesture means to most users, and the same function already
  uses `hop_tab_to_index` at `view.rs:29329` for the "target is a whole block away" case.

Choosing hop changes the absolute positions of the repository run, which touches R5's contiguity
reasoning and would need new tests. That is a product call the plan did not settle.

**Related, unreachable today.** Both `correctness` and `adversarial` independently traced whether a
non-adjacent swap could split a user-created group into non-contiguous runs, and both concluded it
cannot: reaching the far neighbour requires the cursor to be over it, at which point the reassignment
branch intercepts first. The cross-window `ReorderInSource` path guards this with
`swap_would_break_group_contiguity` (`view.rs:29062`); the main in-window path does not. Adding the
same guard symmetrically would close it cheaply if the one-group-per-entry invariant
(`repo_mode_model.rs:783`) is ever relaxed.

### R2. The drag *wiring* is still not covered by any test

`app/src/workspace/view/repo_mode_model_tests.rs` — from `adversarial`, P2, confidence 75, validator
**confirmed**. Partially addressed; the remainder is recorded here.

No test anywhere reaches `on_tab_drag`, `calculate_updated_tab_index`, or
`calculate_updated_tab_index_vertical`. The new suite drives the extracted pure helpers directly. The
consequence the validator confirmed: **deleting the guard clause at `view.rs:29238`, or transposing
the `false`/`true` arguments at `view.rs:29487`/`:29494`, compiles and leaves every test green.**

**What was applied.** `drag_step` now takes `ctx` and calls the real `assign_tab_to_group`
(`view.rs:8055`) instead of writing `group_id` by hand, so the prune path that removes the
"Move to group" route is actually executed. Verified by mutation: reverting
`repo_bound_drag_blocks_reassignment` to `false` now fails 4 tests, including
`test_dragging_a_repositorys_only_tab_leaves_its_group_intact`.

**What remains.** The call sites themselves. Closing it needs one of:

- Make the rect lookup injectable — give `calculate_updated_tab_index[_vertical]` a
  `neighbor_rect: impl Fn(usize, bool) -> Option<RectF>` parameter, with production passing a closure
  over `neighbor_drag_rect`. A unit test can then feed synthetic rects and assert the returned index.
  This changes a production signature, which is why it was not done autonomously.
- Or add an integration test under `crates/integration`, where a real frame populates
  `element_position_by_id_at_last_frame`.

KTD5 recorded the position-cache constraint but did not settle which way to resolve it.

---

## Validator-rejected — recorded so they are not re-raised

- **`neighbor_drag_rect` makes repo-mode reorder a silent no-op** (`correctness`, P2/75). Premise
  fails: repo-bound groups are created expanded (`tab_group.rs:46`) and `collapsed` is only set by
  `toggle_tab_group_collapsed` from a group header, which the flattened repo accordion never renders
  (`vertical_tabs.rs:2015-2036` renders members directly and registers `tab_position_id` at `:2682`).
  `neighbor_in_collapsed_group` is false on that path. The reorder works.
- **Registry `Vec` rebuilt per pointer-move is a real cost** (`performance`, P2/75). `repo_mode_entries`
  does no per-call I/O (FS probes TTL-cached at `repo_mode_model.rs:170-182`), and the identical walk
  already runs at least once per rendered frame via `visible_tab_slot_inputs` ->
  `repo_mode_visible_tab_indices` -> `repo_mode_entry_paths` (`view.rs:28575`) and
  `repo_sidebar.rs:157`. No material added cost.

---

## Residual risks

- `repo_mode_group_section` reads a bound root as loose whenever it is momentarily absent from
  `repo_mode_entries`. In that window `repo_bound_drag_blocks_reassignment` stops blocking, so a drag
  unbinds the tab and prunes the group — permanently. `repo_mode_visible_tab_indices` treats the same
  de-registration as recoverable (falls back to "All"). The two paths draw opposite conclusions from
  the same condition.
- Horizontal bar only: the reorder threshold for a far right neighbour is that neighbour's `min_x()`,
  while `target_group_at_axis` claims a non-source group out to its container `rect.max_x()`
  (`view.rs:29695`). If a group container rect ever extends past the next tab's left edge, one pointer
  event could both be blocked by the guard and satisfy the swap threshold. Not confirmed from the code.
- `on_tab_drag` can now be the first caller to pin `repo_mode_launch_order` via `get_or_insert_with`
  (`repo_mode_model.rs:207-213`). The captured order should be identical — the sort is deterministic
  over the registry — but no test exercises a drag as the first `repo_mode_entries` consumer.
- A drag held open longer than the 5s `REPO_FS_CACHE_TTL` triggers fresh synchronous `stat()` calls
  mid-drag. Pre-existing to the render path; the drag handler is now a second caller.
- `repo_mode_bound_tab_owner` is an exact-path match (`repo_mode_model.rs:1457-1462`), not nested-root
  aware. That is what keeps each repo section mapped to exactly one contiguous group. If nested-root
  ownership is ever added, a section could span two groups and the section-identity reasoning above
  would need revisiting.
- Callsite completeness for `calculate_updated_tab_index[_vertical]`,
  `swap_would_break_group_contiguity`, and `assign_tab_to_group` was established with grep only.

## Testing gaps

- No test drives the cross-window `ReorderInSource` arm now that it passes `entry_paths`, or its
  composition with the existing `swap_would_break_group_contiguity` refusal.
- No test covers `repo_bound_drag_blocks_reassignment` with a repo-bound source **and** a different
  repo-bound target group (repo-to-repo drag). Both guard tests hold one side loose or `None`.
- `test_section_scoped_swaps_keep_every_group_contiguous` only swaps two ungrouped loose tabs across a
  repo run; no test covers a non-adjacent section neighbour belonging to a user-created group.
- No test asserts where the *other* loose tab lands after a non-adjacent swap — the R1 behaviour above.
- The caller-side pinned refusal at `view.rs:29296` is asserted in prose only; the test asserts only
  that `section_neighbor` returns a pinned neighbour.
- U1/U2 name `app/src/workspace/view_tests.rs` as the test file; all tests landed in
  `repo_mode_model_tests.rs` because every scenario needs a registered repository and that file owns
  the fixture. Placement drift, not missing coverage.

## Manual checks still outstanding

These need a running GUI and could not be satisfied in this environment:

- R3 / AE1 / AE2 — the rendered drag gesture inside a repository section.
- R8 / R9 — horizontal-bar behaviour with vertical tabs off.

R6 (detach) and R3's held-state clause were converted to code-verified facts: the detach block ends
`ctx.notify(); return;` at `view.rs:29181`, before `entry_paths`, the guard, and both calculators; and
the active-drag treatment keys on `DraggableState::is_dragging()`, which `on_tab_drag`'s reorder
decision never writes.

## Deferred from planning

- Stranded-tab recovery (product lens): if a tab is ever ejected by some other path, there is no
  in-product route to re-file it under its repository once the group is pruned. Deferred as out of
  scope for this fix.
- Two efficiency observations were deliberately skipped during simplification as not worth the
  indirection.

## Verification note

`./script/presubmit` cannot run in this environment: `command-signatures-v2`'s `build.rs` fails with
`Failed to build command signatures JS: No such file or directory (os error 2)` because `yarn` is not
installed. Confirmed pre-existing by reproducing at `0be44f0d9`. Every reachable sub-gate was run
individually instead: `cargo fmt --check`, `check_no_inline_test_modules`, `cargo clippy -p warp
--all-targets -- -D warnings`, and the full `cargo nextest run -p warp`.

---

# Run 2 — repo-mode post-sync remediation

Branch: `vinh-main` (base `eeed965ef`)
Change: collapsed-row activity rollup + external-editor launch integrity + migration rename
(plan `docs/plans/2026-08-08-001-fix-repo-mode-post-sync-remediation-plan.md`)
Review run: `20260808-110914-e81c4bae`

Reviewers: correctness, security, adversarial, project-standards, testing, maintainability,
performance, reliability, data-migration. Four findings survived mechanical merge; an independent
validator confirmed all four. Three were applied. This section records everything not applied.

**No cross-model peer ran.** The host attests as family `claude`, and same-family exclusion removes
that route; no different-provider CLI was reachable on PATH (`codex`, `grok`, `cursor-agent` all
absent). The in-process adversarial reviewer ran as the sanctioned local fallback. No finding below
carries cross-model corroboration, and no code left this machine.

No tickets were filed, for the same reason as Run 1: this repository's issue tracker belongs to the
upstream OSS project, and filing there for an unmerged personal branch was not authorized.

## Not applied — needs a decision

### R1. Non-UTF-8 macOS path launches JetBrains on a file that does not exist, and reports success

`app/src/util/file/external_editor/mac.rs:240` — from `security`, `reliability`, and `adversarial`
independently, P2, confidence 75, validator **confirmed**. Owner: human.

A user whose file lives on a volume macOS does not police for UTF-8 (exFAT, FAT32, SMB, NFS) asks
Warp to open it in a JetBrains IDE. `to_string_lossy()` replaces every invalid byte with U+FFFD, so
the argument names no file on disk. `Editor::open` (`mac.rs:255-313`) treats a spawned process as
success, so `open_file_path_with_line_and_col` (`mac.rs:354-358`) returns `OpenOutcome::Editor`, the
Finder fallback never runs, and no warning or `report_error!` is emitted. For the folder flow this
even records an `OpenedFolderInIde` telemetry hit.

**Why it was not applied.** The plan deferred it deliberately — KTD6 plus the Scope Boundaries entry
"The macOS non-UTF-8 path in `command_executable_and_arguments` and `jetbrains_command`". The finding
does not dispute the deferral; it disputes the deferral's stated cost. This change set removed a panic
here, which is strictly better than crashing, but the replacement failure is silent — the exact class
the Linux half of this same change set (`NoTargetPath`, R7) was built to close. Two platforms now
answer the same question two different ways.

Closing it is cheap and reuses plumbing that exists: guard in `Editor::open`, return `false`, and the
existing Finder fallback fires. One caveat found during review — `Window::open_file_path`
(`crates/warpui/src/platform/mac/window.rs:750-757`) silently no-ops on a non-UTF-8 path, so the
fallback may not be visible either. Both halves would need fixing for the user to see anything.

### R2. R5 hides the rollup on dead rows that may still own live tabs

From the planning-stage product lens, P1, never implemented. Owner: human.

R5 as written says a dead repository row shows no rollup, and the code implements exactly that
(`is_dead` overrides). The product lens argued the opposite default: a dead entry is one whose path
no longer resolves, but its bound tabs can still be live and still accrue unread agent activity.
Hiding the rollup there hides real state on the one row a user is least likely to expand.

This was not folded into the plan because flipping it changes what "dead" means on the sidebar and
touches the dead row's existing trailing-edge construction (the `Expanded` spacer plus the "Remove"
button), which is where a rollup would have to go. A product call, not a defect.

## Pre-existing — not introduced by this change

Recorded so they are not re-raised as regressions.

- **Absolute file paths logged at `info` in the Sublime open path reach Sentry breadcrumbs**
  (`app/src/util/file/external_editor/linux.rs:620`, `security`, P2/75). Untouched by this change set.
- **macOS JetBrains launch silently drops the requested column** (`mac.rs:244`, `correctness`,
  P3/50). The line argument is built and passed; the column is not.

## Deliberately skipped during simplification

- Gating `any_synced` on `show_indicators` inside `repo_mode_activity_by_entry` (efficiency lens).
  Rejected: it pushes a UI setting into a sweep that is deliberately setting-agnostic — the rollup
  answers to tab state, not to badge configuration — and it regresses the loop's early exit.

## Residual risks

**Demoted from primary during review** (single reviewer, anchor 50 — real, below the reporting bar):

- `NoTargetPath` routes a user-environment condition through `report_error!` at `linux.rs:673` and
  `linux.rs:705`, so a `.desktop` entry with no path field code pages Sentry on every open attempt,
  while the JetBrains/Sublime arms only `log::warn!` the identical error type.
- `repo_mode_activity_by_entry` calls `pane_group.visible_terminal_views(app)` per bound tab per
  render, allocating a `Vec` each time — roughly 100 small allocations per frame at ~30 repos /
  ~50 tabs. Well under the frame budget, but a lazy `.any()` over `visible_pane_ids()` would keep
  the same short-circuit without the collect.
- `repo_row_indicators` hardcodes `is_terminal_row: true`, so a repo-bound tab whose pane group has
  no visible terminal view would still contribute the synced-inputs link icon. Matches `tab.rs:1005`,
  which also skips the terminal-ness check.
- `render_entry_row` wraps every live row's title line in `Expanded::new(1., ...)`, not only rows
  that have an indicator, so R6's "renders exactly as today" is a pixel claim rather than a
  construction claim. The maintainability reviewer traced warpui_core's Flex/Expanded layout and
  found no visible difference in the no-indicator case; no reviewer found evidence of a regression.

**Behavior changes with no test:**

- Attached field codes now fail. `strip_prefix('%')` matches standalone tokens only, so a
  non-conformant `Exec=/usr/bin/editor --goto=%f` now returns `NoTargetPath` and sends the user to
  the file manager where it previously launched the editor with a literal argument.
- How often real `.desktop` entries lack a path field code — the population R7 newly routes to the
  file-manager fallback — is unverified. Every entry in the repo's test corpus carries one.

**Structural:**

- `args.first().ok_or(DesktopExecError::NoExec)?` (`linux.rs:189`) is now unreachable from
  `build_command`: `args` can only be empty when `substituted_path` is false, which returns earlier.
- `repo_mode_badges_by_entry` and `repo_mode_activity_by_entry` are two independent full tab /
  pane-group sweeps per frame when badge settings are on — a deliberate, documented tradeoff, but
  worth re-measuring with a profiler if repo-mode usage grows well past ~30 repos / ~50 tabs. The
  cost assessment in review was static allocation counting, not a measured frame-time profile.
- Callsite completeness for the private-to-`pub(super)` visibility widenings was established with
  grep only.

**Pre-existing, restated because this change set touches the same paths:**

- `.desktop` Exec strings are trusted wholesale. `compute_installed_editors` (`linux.rs:481`) walks
  `default_paths()`, which includes user-writable `~/.local/share/applications` and env-controlled
  `XDG_DATA_DIRS`, and `build_command` execs `args.first()` with no allowlist. Inherent to the
  freedesktop design, not widened here. `Exec=%f` alone still makes the clicked file the program.
- `%u`/`%U` canonicalize (`linux.rs:234`) after the `is_file()`/`is_dir()` gate — a TOCTOU window
  where a symlink swap redirects the `file://` URL. Standard for desktop file-open, unchanged.
- CI's database-migration job only runs against a fresh empty DB and diffs `schema.rs`; it never
  exercises the upgrade path against an already-migrated fixture. The rename's upgrade-safety rests
  on static analysis of vendored `diesel_migrations` 2.3.1 / `migrations_internals` 2.3.0, plus
  `crates/integration/tests/data/three_tabs.sqlite` — a fixture that predates this migration.

## Testing gaps

- No test asserts R7's user-visible contract end to end: that `open_file_path_with_line_and_col`
  returns `OpenOutcome::FileManager` and records no editor launch when `build_command` fails. Every
  new Linux test stops at `Err(NoTargetPath)`.
- Nothing pins what the new failure path emits. A future change interpolating the target path into
  the `report_error!` message would not be caught — the exact regression class `eeed965ef` fixed.
- No Linux test for a field code embedded in a larger token (`--goto=%f`), which `NoTargetPath` now
  rejects, or for an Exec whose first token is a path field code (`Exec=%f`).
- macOS: the non-UTF-8 test asserts U+FFFD substitution happens, but nothing asserts what the user
  gets — that is the deferred R1 above.
- R2's window-wide `SyncedPanes::All` clause is asserted nowhere.
- No test covers a bound tab with no visible terminal view while sync-inputs is on — the exact input
  separating the rollup's hardcoded `is_terminal_row = true` from the nested rows' pane-type check.
- `test_unread_activity_is_found_in_a_later_tab_of_the_same_entry` never sets synced inputs, so the
  `any_unread && any_synced` short-circuit is never exercised with one flag already true.
- R6 / KTD4's "renders exactly as today" is asserted only at `repo_row_indicators`. Nothing asserts
  the rendered element tree for an all-false live row, which is where the unconditional `Expanded`
  diverges. This codebase's test style has no Element-tree assertion facility.
- Nothing pins `persistence::MIGRATIONS`'s derived version list against a directory rename that
  changes only the descriptive suffix.
- No automated check enforces the AGENTS.md comment-duplication and caller-enumeration conventions;
  `./script/format` and clippy do not lint doc-comment content, so both are manual-review only.

## Verification note

`./script/presubmit` still cannot run here — `yarn` is absent (same failure as Run 1), and
`clang-format` is not installed either, though no C/ObjC file is in this change set. The reachable
sub-gates were run individually and are green: `./script/format --check`, `cargo clippy -p warp
--all-targets --tests -- -D warnings`, and `cargo nextest run -p warp --lib --no-fail-fast`
(6432 passed, 6 skipped).

**U4 is unverifiable on this host.** `app/src/util/file/external_editor/linux.rs` and
`linux_tests.rs` are `#[cfg(any(target_os = "linux", target_os = "freebsd"))]`, and the only
installed Rust target here is `aarch64-apple-darwin`. Those files were reviewed by reading and
checked with `rustfmt --check` (which proves they parse), but they have not been type-checked or
executed. **Linux CI is the only gate on them, including the `test_exec_with_a_non_utf8_path_errors`
test added during this review pass.**
