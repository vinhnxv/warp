# Residual review findings — `vinh-main`

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
