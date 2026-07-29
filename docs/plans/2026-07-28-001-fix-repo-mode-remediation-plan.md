---
title: Repo Mode Remediation - Plan
type: fix
date: 2026-07-28
topic: repo-mode-remediation
artifact_contract: ce-unified-plan/v1
artifact_readiness: implementation-ready
product_contract_source: review-findings
execution: code
deepened: 2026-07-30
---

# Repo Mode Remediation - Plan

## Goal Capsule

- **Objective:** Close the defects found in the 2026-07-28 review of the three repo-mode features by removing the *structures* that produced them, not by patching the individual symptoms. Where a class of bugs shares one root cause, this plan replaces the root cause once.
- **Product authority:** The three shipped plans (`2026-07-19-001` sidebar, `2026-07-20-001` open-folder, `2026-07-23-001` remote-SSH) as amended on 2026-07-28. This plan introduces no new product surface; it makes the code match those contracts and removes the fragility that let them drift. Owner: Vinh Nguyen.
- **Execution profile:** Eleven units. Nine are grouped by root cause (RC1–RC9); U10 and U11 collect the confirmed defects that share no cause. U1–U3 are the tab-rendering core and must land in order. U4–U5 are the remote lifecycle and must land in order. U6 gates U7 and U8 (external editor). U9 (activation) depends on U3. U10 (sidebar affordances) and U11 (residual defects) are independent of everything, and U11 lands last. Every unit is independently shippable and independently revertable.
- **Stop conditions:** If a unit's fix requires touching an upstream file outside the sidebar plan's amended R9 inventory, stop and record it rather than widening silently. If U1 or U2 turns out to require changing upstream's tab-group data model rather than its consumers, stop — that is a different plan and probably an upstream PR.
- **Open blockers:** None. Three items were resolved during planning: the attribution decision (KTD3), the property-test dependency and the persisted-unverified-row reconciliation (both under Assumptions / U4, resolved 2026-07-30).

- **Already fixed, out of scope here:** the `remote_ssh_command` shell injection and the last-tab close-confirmation loss, both shipped 2026-07-28 (`827b3a84a`, `f201d806e`). Referenced below only where a unit must not regress them.

---

## Product Contract

### Summary

The three repo-mode features work, and the review confirmed most of the design. What it also found is that several defects are not independent: six remote-add bugs come from one missing ownership model, four tab-bar rendering defects come from one duplicated slot computation, and three external-editor defects come from one overloaded field. Patching thirty symptoms would leave all three structures in place to generate the next thirty. This plan removes the structures.

### Problem Frame

The review produced ~30 findings across three features. Grouped by cause rather than by symptom, they collapse into nine:

| # | Root cause | Symptoms it produced |
|---|---|---|
| RC1 | Two divergent implementations of "which tabs are visible, and how do they group" | horizontal bar renders a hidden tab and drops a visible one; `is_first_in_bar` wrong under filter; `TabBarState.tab_count` counts hidden tabs |
| RC2 | Group contiguity is an *emergent* invariant that non-render consumers assume is *enforced* | `move_group_block` drains a range containing non-members — silent data corruption; `ReorderInSource` violates contiguity by design |
| RC3 | Tab attribution reads live cwd every frame, contradicting R12/AE6/KTD6 | selecting a repo opens a redundant terminal; display contradicts the acceptance example; per-frame `stat`-adjacent work |
| RC4 | The remote add mutates the registry speculatively and has no notion of "is this operation still wanted" | probe-after-remove resurrects the entry; cancel + late success registers a cancelled entry; re-add + failure destroys the existing entry and orphans its group; stale success closes a reopened modal; unbounded concurrent reprobes with last-writer-wins; probe map grows unboundedly |
| RC5 | `format_remote_key` is a partial function presented as total | `~/path` and relative paths produce keys that cannot be parsed back; the row renders dead with a raw URI label; `[::1]` typed by hand produces a corrupt key; the full connection string is logged on every frame for such a key |
| RC6 | `executable_path` was a detection sentinel promoted to a launch target without validation | "Open folder in VS Code" silently does nothing on Windows; spawn failure is a silent no-op on Windows/Linux; telemetry records launches that failed |
| RC7 | Editor detection has no caching discipline | ~26 synchronous LaunchServices lookups per window at startup on the UI thread; ~104 on opening Settings; a cloud settings sync re-triggers them for users who have the feature off |
| RC8 | The default-folder-editor resolver has no representable "no editor" state reachable from the UI | an uninstalled default silently substitutes a different IDE; installing an IDE silently changes the user's default; "reveal in Finder" is specified but unreachable |
| RC9 | Tab activation has no single chokepoint, so the repo filter is applied at some entry points and not others | Ctrl+Tab / Cmd-N / close-tab can activate a tab the filter hides, leaving a strip with no active tab |

### Key Decisions

- **KD1. Fix causes, not symptoms.** A unit is scoped to a root cause. Where fixing the cause deletes a symptom outright, that is preferred over fixing the symptom.
- **KD2. Prefer deleting a dependency over strengthening it.** RC1/RC2 could in principle be addressed by enforcing contiguity harder. They are instead addressed by making rendering not need contiguity at all. An invariant that rendering does not depend on is one fewer thing to break.
- **KD3. Bound tabs attribute by their binding, not their cwd.** Decided during planning; see KTD3 for the reasoning and what it does and does not fix.
- **KD4. Nothing is written to the registry on speculation.** The registry is persistent user data. An operation that has not succeeded does not get to mutate it.

### Requirements

**Tab rendering**

- N1. The horizontal tab bar and the vertical tab panel derive their grouped layout from **one** function over the filtered visible-tab list. Neither reconstructs a dense index range from a start plus a length.
- N2. Rendering a tab group is correct regardless of whether that group's members are contiguous in `Workspace::tabs`.
- N3. Chrome that varies by position or count under a filter — first-in-bar styling, single-member group treatment, tab counts — is computed from the visible set, not the full tab list.
- N4. Consumers that genuinely require contiguity (`group_member_index_range` and its callers) behave safely rather than corruptingly when it does not hold, and a violation is reported rather than silently tolerated. Reporting must not abort: the non-contiguous state is reachable by design (see KTD2), so a bare `debug_assert` in the shared accessor is not an acceptable mechanism.

**Tab attribution and activation**

- N5. A tab bound to a group displays under that group's entry for as long as it is bound, regardless of its current working directory. This restores R12/AE6/KTD6 of the sidebar plan as originally written.
- N6. Every activation path — click, keyboard, close-tab fallback, tab-number, next/prev, last — leaves the active tab inside the visible set, or collapses the selection so that it is.

**Remote add lifecycle**

- N7. A remote entry is written to the registry only when its probe has succeeded. A probe that fails, times out, is cancelled, or resolves after the user removed or replaced the operation leaves persistent state exactly as it found it.
- N8. At most one probe is in flight per registry key. A result belonging to a superseded operation is discarded, not applied.
- N9. Cancelling or closing the modal invalidates the in-flight operation for both outcomes, success and failure alike.
- N10. Removing an entry — by any path, including a failed re-add — clears every piece of state that referenced it: registry row, probe state, launch order, bound group, and the selection if it pointed there.

**Remote key identity**

- N11. `parse_remote_key(format_remote_key(x)) == x` for every input the form can produce, including `~`, `~/path`, relative paths, and bracketed IPv6 literals. This is verified by generated coverage over the input grammar, not by enumerated examples.
- N12. A key already persisted in a form the current parser rejects is readable — it either parses or is repaired on read, and in neither case does it render as a raw URI or log the user's connection string.

**External editor**

- N13. Detecting that an editor is installed and knowing how to launch it are separate concerns with separate fields. An editor is reported installed only if its launch target exists.
- N14. A launch that fails falls back to revealing the folder in the OS file manager, and telemetry records what actually happened.
- N15. "No external editor — reveal in the file manager" is a state the user can select in Settings, and is what an explicitly-chosen-but-uninstalled editor falls back to. The default is never silently changed by installing or uninstalling an application.
- N16. Editor detection does not run synchronously on the UI thread during window construction, and does not run at all for users with the feature disabled.

**Preserved**

- N17. Every fix preserves the two 2026-07-28 security/data-loss fixes: the interactive `ssh` line quotes every user-entered field, and the last-tab close still confirms in repo mode.
- N18. Flag-off parity (sidebar R8/AE3) holds at every unit boundary.

**Sidebar affordances and residual defects**

These two requirements exist so U10 and U11 are traceable. They introduce no product surface beyond the "individually-confirmed defects" already admitted by Scope Boundaries; they name what those units must achieve so a reviewer can check them.

- N19. No single click destroys registry state. Removing an entry is its own hit target, and the render path performs no filesystem access, no unbounded per-frame recomputation, and no unbounded cache growth.
- N20. A tab opened with a repo selected receives a path its own shell can resolve; a probe failure is classified as the thing that actually failed; and a silently-degraded remote connection tells the user rather than presenting a connected tab that never arrived.

### Acceptance Examples

- NA1. **Covers N1/N2.** Given group G owns tabs at indices 0 and 2 and index 1 holds a tab the filter hides, when the horizontal bar renders with G selected, then it renders exactly the tabs at 0 and 2 and does not render the tab at 1.
- NA2. **Covers N5.** Given `/repo` and `/repo/sub` are both registered and a tab bound to `/repo` has cd'd into `/repo/sub`, when the sidebar renders, then the tab appears under `/repo`; and when `/repo` is selected, no new terminal is opened.
- NA3. **Covers N6.** Given repo A is selected and a loose tab exists, when the user presses Ctrl+Tab onto the loose tab, then either the tab strip shows it or the selection collapses to All — never a strip with no active tab.
- NA4. **Covers N7/N9.** Given the user submits a remote add and presses Escape while the probe is in flight, when the probe later succeeds, then nothing is registered, no modal is closed, and the projects table is byte-identical to before the submit.
- NA5. **Covers N7/N10.** Given a resolved remote entry exists and its host is unreachable, when the user re-adds the identical connection and the probe fails, then the existing entry, its bound group, and its tabs are untouched.
- NA6. **Covers N8.** Given the user selects the same unreachable remote row three times in six seconds, when all three probes resolve, then at most one `ssh` subprocess ran at a time and the row shows the newest result.
- NA7. **Covers N11.** Given a form with remote path `~/projects/app`, when the key is formatted and parsed back, then every field round-trips, and the pending row shows the host and a connecting state — never a raw `ssh://…` string.
- NA8. **Covers N13/N14.** Given VS Code is the default folder editor and its launch target does not exist, when the user clicks Open folder, then the folder is revealed in the file manager and telemetry records `"finder"`, not `"VS Code"`.
- NA9. **Covers N15.** Given the user selected Zed and then uninstalled it, when they click Open folder, then the folder is revealed in the file manager and Settings shows no editor selected — not a different IDE.
- NA10. **Covers N16.** Given the feature flag is off, when a cloud settings sync arrives, then no editor detection runs.
- NA11. **Covers N19.** Given a registered repo whose path is temporarily unreachable, when the user left-clicks its row anywhere outside the Remove control, then the registry entry still exists.
- NA12. **Covers N20.** Given a WSL shell is configured and a host repo root is selected, when a new tab opens, then the tab's starting directory is one the WSL shell can resolve — not the host path verbatim.
- NA13. **Covers N7/N12.** Given the `projects` table already holds a remote key persisted by the pre-U4 speculative path and never verified, when the sidebar next renders, then that row does not appear as a resolved entry and its connection string is not logged.

### Success Criteria

- The four confirmed high-severity defects (tab-bar mis-render, redundant terminal on select, Windows launch no-op, silent spawn failure) are gone, each verified by a test that fails on the current code.
- The remote-add lifecycle has one state machine rather than five ad-hoc guards, and the six lifecycle bugs are covered by tests driving that machine directly.
- No amendment in the three plans still reads "resolution pending".

### Scope Boundaries

**In scope:** the nine root causes above, and the individually-confirmed defects that attach to them.

**Deferred, recorded, not fixed here:**

- The `Modal` overlay not blocking input to the view behind it in release builds (`EventDispatchMode::Broadcast`). Generic to every Warp modal, not introduced by repo mode; U4 removes repo mode's exposure to it, but the general issue belongs upstream.
- `find_git_repo` stopping at `$HOME`, so a repo rooted at or above `$HOME` is never detected. Pre-existing `repo_metadata` limitation.
- Repo detection never firing for a tab's very first block metadata (`if let Some(prev) = active_block_metadata.take()`). Pre-existing; low impact because the button is disabled in that window anyway.
- The editor logo SVGs not being theme-aware (hardcoded fills, low contrast on dark themes). Cosmetic; needs a design call on tinting versus per-theme assets.
- `RepoMode` on with `GroupedTabs` off destroying repo↔group bindings. Not reachable in any shipped configuration (both features live in the same `default` bundle and nothing can diverge them at runtime). Guard added in U3 as a one-line assertion, but no behavioral work.
- Upstreaming `crates/warp_assets/build.rs`, which is a genuine upstream fix.

**Out of scope:** any new user-facing capability. This plan adds no feature.

### Dependencies / Assumptions

- `vertical_tabs.rs`'s existing slot computation (`take_while` over `visible_tabs` carrying real member indices) is correct and is the model U1 generalizes. Verified during review.
- Upstream's `tab_bar_slots` is sound on master because without a filter, "consecutive while iterating" and "consecutive in `self.tabs`" coincide. U1 therefore changes a repo-mode-introduced regression, not upstream behavior — flag-off rendering must be byte-identical.
- The `projects` table is the only persistent home for registry entries; `repo_mode_remote_probes` is ephemeral by design (remote R11).
- **Verified 2026-07-30:** the speculative write U4 removes is real and reachable — `add_remote_repo_mode_entry` upserts `pending_key` into `projects` *before* `spawn_remote_probe`. Any dogfood build that has submitted the form therefore already holds unverified remote keys in persistent storage, including keys in the shape U5 cannot parse. Removing the speculative write does not clean them up; U4 must (see its read-side reconciliation step and NA13).

---

## Planning Contract

### Key Technical Decisions

- **KTD1 — One slot model, member indices, shared by both surfaces.** Replace `TabBarSlot::Group { first_index, run_len }` with a variant carrying the visible member indices. Build slots in one function that takes the filtered index list; both `tab_bar_slots` and `vertical_tabs.rs`'s `render_groups` consume it. This is what the sidebar plan's KTD3 already required ("Both horizontal strip and vertical list consume the same accessor so AE2 cannot leak via search") — honored at the visible-index layer and skipped at the slot layer, which is precisely where the divergence appeared. Consequence worth stating: after this, rendering has no contiguity dependency, so N2 is satisfied structurally rather than by discipline.

- **KTD2 — Contiguity becomes observable, and its one corrupting consumer becomes safe.** `group_member_index_range` documents "the workspace enforces this invariant" and nothing does. Correct the doc comment, and change `move_group_block` to move the actual member indices rather than draining `first..=last` — today an intruding non-member is silently absorbed into the group, which is data corruption rather than a rendering glitch. Separately, `DragResult::ReorderInSource`'s bare `self.tabs.swap` is the one site that violates contiguity by design; make it either reposition or refuse. This unit is upstream-shaped and is the best candidate in this plan for an upstream PR.

  **Report the violation; do not assert on it.** An earlier draft of this decision added a `debug_assert` to `group_member_index_range` that the returned span contains only members. That is wrong, and the rest of this plan is why: the non-contiguous state `[T0(G), T1(loose), T2(G)]` is reachable — KTD3 relies on it being reachable to explain the tab-bar mis-render, and no mutation site enforces otherwise. The accessor has 23 references in `view.rs`, so an assert there converts a rendering-layer glitch into a debug-build abort on an ordinary user action. Use `report_error!` (Sentry, non-fatal) instead, so the state is visible in telemetry without aborting, and let the rendering path stay correct by construction via U1. Callers that genuinely require a contiguous span validate locally and take a safe branch rather than trusting the accessor.

- **KTD3 — Bound tabs attribute by `bound_root`; cwd attribution is removed.** `repo_mode_bound_tab_owner` currently buckets a bound tab under the deepest registered entry that is an ancestor of its *live* cwd. Return the bound root instead. This restores sidebar R12/AE6/KTD6 verbatim, removes a `canonical_session_pwd_if_local` read per tab per frame, and deletes the redundant-terminal defect outright (an entry's tabs can no longer be attributed away from it, so `select_repo_mode_entry` always finds a member and never falls through to `create_repo_mode_group_with_tab`).

  **It does not fix the tab-bar mis-render, and this plan does not rely on it to.** An earlier reading held that it would; verification showed otherwise. Group contiguity is emergent, not enforced, so `[T0(G), T1(loose), T2(G)]` is reachable, and with G selected the loose tab is filtered out and the run-merge spans it — no cwd drift required. KTD1 is the fix; KTD3 narrows the input space and is justified on its own terms.

  Cost: `test_bound_tab_owner_rules` currently pins the opposite behavior and must be inverted. Behavior change to accept: a tab that cd's into another registered repo stays listed under the repo it was opened from.

- **KTD4 — One probe session per key, with a generation counter, replacing five ad-hoc guards.** Today: the entry is upserted before the probe starts; the failure arm checks a modal token; the success arm checks only `token.is_some()`; reprobe has no guard at all; removal does not cancel anything. Replace with a single `RemoteProbeSession` keyed by registry key, holding a monotonically increasing generation. Every spawn captures its generation. Every result checks `session.generation == captured` before touching anything, and drops silently otherwise. Cancel, close, remove, and re-add all bump the generation. The registry is written **only** in the success arm of a live generation (KD4/N7).

  This one change subsumes six findings: resurrection-after-remove, cancel-then-phantom-registration, re-add-destroys-existing, stale-success-closes-modal, unbounded concurrent reprobes with last-writer-wins, and unbounded growth of the probe map. It also removes the second reachability path for the injection fixed in `827b3a84a` — there is no longer a persisted-but-unverified row to click.

- **KTD5 — Make the key codec total, and tolerant on read.** `format_remote_key` concatenates `:{port}{path}` assuming `path` starts with `/`; `parse_remote_key` splits the authority at the first `/`. Neither assumption holds for `~`, `~/x`, or a relative path — which is the documented happy path (remote R3) and the modal's own placeholder. Fix the codec so the round-trip is total, and prove it with generated coverage over the input grammar rather than a fixed example set; the enumerated tests are exactly what missed this. Because unparseable keys may already be persisted on this dogfood build, the read path repairs rather than rejects, and the "unparseable" branch stops logging the connection string and stops rendering a raw URI as a display name. Normalize `[::1]` to `::1` on input while here.

- **KTD6 — Split `EditorMetadata::executable_path` into a detection probe and a launch target.** The field began as a registry-derived existence sentinel with `#[allow(unused)]` and was promoted to a `Command::new` argument by the open-folder work, which is how `<InstallLocation>\bin\code.exe` — a path VS Code does not ship — became the Windows launch command. Two fields, two jobs; `compute_installed_editors` verifies the launch target exists before reporting an editor installed. Then make failure honest: a failed spawn falls through to reveal-in-file-manager (macOS already does this; Windows and Linux `return` past it), and telemetry reports the outcome rather than the intent.

- **KTD7 — Give the folder-editor setting a representable "none", and make the seed non-destructive.** `resolve_default_folder_editor_with_installed`'s uninstalled-editor arm falls through to a seed returning `SUPPORTED_EDITORS.first()`, so uninstalling your chosen IDE silently re-points the button at another one, and the Settings dropdown then displays the substitute as your selection. Meanwhile the dropdown emits no "None" row, so the `Warp | EnvEditor | SystemDefault => None` arm — the "reveal in Finder" fallback that KTD5 of the open-folder plan specifies — is unreachable from the UI. Add the row, return `None` for an explicitly-chosen-but-uninstalled editor, and treat the seed as a first-run suggestion that is persisted once rather than a value recomputed on every read.

- **KTD8 — One lazily-populated, explicitly-invalidated editor cache, off the UI thread.** The current cache is filled by a synchronous ~26-editor LaunchServices sweep at the tail of `Workspace::new`, per window, and the `EditorSettings` subscription rescans before checking the feature flag — so a cloud sync of an unrelated setting costs the sweep for users who have the feature off. Populate lazily on first need, refresh in the background, check the flag first, and stop cloning the whole `Vec<Editor>` per call.

- **KTD9 — Activation gets one chokepoint.** `sync_repo_mode_selection_to_active_tab` documents an invariant ("the filtered tab strip always contains the active tab") but is wired only to `WorkspaceAction::FocusPane`. Every other path — `ActivateTab`, `ActivateTabByNumber`, next/prev, last, and `remove_tab`'s post-close activation — funnels through `activate_tab_internal`. Call it there and delete the special case. An invariant with one enforcement point is maintainable; one with seven is not.

### High-Level Technical Design

Dependency graph:

```
U1 (slot model) ──▶ U2 (contiguity consumers)
     │
     └──▶ U3 (attribution)  ──▶ U9 (activation chokepoint)

U4 (probe session) ──▶ U5 (key codec)

U6 (editor detect/launch) ──▶ U7 (editor cache)
                          └──▶ U8 (editor setting "none")

U10 (sidebar affordances)   — independent
U11 (residual defects)      — independent, last
```

U1→U2 is ordered because U2's reporting will fire on states U1 makes harmless to render but still invalid to `move_group_block` — landing U2 first would emit telemetry for a rendering bug U1 has not fixed yet. U4→U5 is ordered because the codec repair path needs the session model to decide what to do with a repaired key mid-probe. U6 gates U7 and U8 because both consume the field U6 splits — caching or defaulting on the pre-split field would have to be redone. U3 gates U9 because the chokepoint's invariant is stated over the attribution U3 changes.

### Assumptions

- `TabBarSlot` is internal to `app/src/workspace/`; changing its shape does not ripple outside the amended R9 inventory. To be confirmed in U1's first step; if false, that is a stop condition.
- ~~A property-test dependency is either already in the workspace or acceptable to add.~~ **Resolved 2026-07-30:** neither `proptest` nor `quickcheck` appears anywhere in the workspace, including `Cargo.lock`. U5 therefore takes the fallback arm by default — a generated input table over the documented grammar, built in `crates/repo_mode`'s own test module with no new dependency. Adding `proptest` as a `[dev-dependencies]` entry on `crates/repo_mode` remains acceptable if the generated table proves unwieldy; it is a leaf crate and the dependency is test-only. Either way the requirement is generated coverage, not enumerated examples.

---

## Implementation Units

### U1. One slot model shared by both tab surfaces

**Covers:** RC1, N1–N3. **Files:** `app/src/workspace/view.rs` (`tab_bar_slots`, `render_horizontal_tab_group`, `TabBarState`, `is_first_in_bar`, `group_has_single_member` call sites), `app/src/workspace/view/vertical_tabs.rs` (`render_groups`), `app/src/workspace/view_tests.rs`.

**Approach:** Change `TabBarSlot::Group` to carry the visible member indices instead of `first_index` + `run_len`. Extract the slot-building loop into one function taking the filtered index list, modeled on `vertical_tabs.rs`'s existing `take_while` over `visible_tabs`. Point both surfaces at it. `render_horizontal_tab_group` iterates the carried indices instead of reconstructing `first_index..first_index+run_len`. Recompute `is_first_in_bar` from slot position rather than `first_index == 0`, `TabBarState.tab_count` from the visible count, and `group_has_single_member` from the slot's member count.

**Tests:** NA1 as a unit test on the slot builder — group members at 0 and 2 with 1 filtered out yields a slot carrying exactly `[0, 2]`. A flag-off test asserting slot output is unchanged from the pre-U1 shape for an unfiltered tab list (N18). A test that a group whose members are non-contiguous with nothing filtered still renders every member exactly once.

### U2. Make contiguity checked, and its corrupting consumer safe

**Covers:** RC2, N4. **Files:** `app/src/workspace/view.rs` (`group_member_index_range`, `move_group_block`, `DragResult::ReorderInSource` arm), `app/src/workspace/view_tests.rs`.

**Approach:** Correct `group_member_index_range`'s doc comment from "the workspace enforces this invariant" to what is actually true — the run is assumed contiguous and nothing enforces it — and emit a non-fatal `report_error!` when the returned span contains a non-member, so the state is visible in telemetry. Do **not** add a `debug_assert` here: the state is reachable and the accessor has 23 references, so an assert aborts debug builds on ordinary use (KTD2). Change `move_group_block` to move the group's real member indices rather than draining `first..=last`, so an intruding non-member is left in place instead of being absorbed. In the `ReorderInSource` arm, the bare `self.tabs.swap` deliberately skips group reassignment; make it refuse the swap when it would place a non-member inside a group's span, which is the minimal change that preserves the arm's stated intent.

**Tests:** `move_group_block` on `[T0(G), T1(none), T2(G)]` leaves `T1` outside the group. `ReorderInSource` on the same shape refuses rather than splitting. A test that constructs the non-contiguous state directly and drives the render path plus `group_member_index_range` **without panicking** — this is the regression guard for the assert-versus-report decision, and it fails if someone reintroduces a hard assert.

**Note:** this unit is upstream-shaped — the defects exist on master, they are simply unreachable there through the repo-mode path. Worth proposing separately.

### U3. Bound tabs attribute by their binding

**Covers:** RC3, N5. **Files:** `app/src/workspace/view/repo_mode_model.rs` (`repo_mode_bound_tab_owner`, `repo_mode_tab_partition`, `select_repo_mode_entry`), `repo_mode_model_tests.rs`.

**Approach:** `repo_mode_bound_tab_owner` returns `bound_root` for any tab that has one; cwd is consulted only for unbound tabs, and only to the extent the current code already does for them. Drop the now-unused per-frame `canonical_session_pwd_if_local` read on the bound path. Invert `test_bound_tab_owner_rules`. Verify `select_repo_mode_entry`'s fall-through to `create_repo_mode_group_with_tab` is now unreachable for an entry that has tabs, and add the guard anyway (defense against a future attribution change reintroducing it).

**Tests:** NA2, both halves — display and no-redundant-terminal. A test that selecting an entry with existing tabs opens no new terminal, which fails on current code.

**Plan bookkeeping:** remove the "resolution pending" language from the sidebar plan's amended AE6 and KTD6; they are satisfied as originally written.

### U4. One probe session per remote key

**Covers:** RC4, N7–N10. **Files:** `app/src/workspace/view/repo_mode_model.rs` (`add_remote_repo_mode_entry`, `apply_remote_probe_result`, `spawn_remote_probe`, `reprobe_remote_entry`, `drop_pending_remote_entry`, `replace_registry_key`, `remove_repo_mode_entry`), `remote_connection_modal.rs` (lifecycle), plus tests in both.

**Approach:** Introduce a probe-session map keyed by registry key, each holding a generation counter and the current state. Spawning captures the generation; every callback checks liveness before acting and drops silently otherwise. Cancel, modal close, entry removal, and re-add all bump the generation. Skip spawning while a live probe exists for the key. Move the `upsert_project` out of the submit path into the success arm — the pending row lives in the session map and is projected into the list, never persisted (this is what remote R9 and R11 already describe). `drop_pending_remote_entry` gains the cleanup `remove_repo_mode_entry` already does: selection, bound group, launch order, probe state.

**Reconcile what the old path already persisted.** Removing the speculative write fixes the source but not the damage: this branch has been running with `add_remote_repo_mode_entry` upserting `pending_key` before the probe, so persisted-but-never-verified remote keys already exist on dogfood installs. Once the registry means "verified", such a row is a lie the new code has no state for. Reconcile on read rather than migrating on write — a remote key with no probe session is displayed as unresolved (not resolved, not dead) and is reprobed when the user touches it, which is exactly the R11 refresh-by-use rule already specified. Do not silently delete these rows; the user typed those connection details and an entry vanishing on upgrade is worse than one showing "not connected". Coordinate with U5: a row in this state may also carry an unparseable key, and the two repairs must not fight — U5 owns the key shape, U4 owns the liveness state.

**Tests:** NA4, NA5, NA6 each as a direct test on the session machine with a stubbed probe. NA13 for the persisted-unverified row. Plus: a success arriving for a removed key registers nothing; a success arriving after the modal reopened for a different host does not close it; two probes for one key resolving out of order leave the newer result. `apply_remote_probe_result` is already `pub(super)` and drivable, so none of this needs UI plumbing.

### U5. Make the remote key codec total

**Covers:** RC5, N11–N12. **Files:** `crates/repo_mode/src/entry.rs` (`format_remote_key`, `parse_remote_key`), `entry_tests.rs`, `repo_mode_model.rs` (`remote_list_entry` unparseable branch), `remote_connection_modal.rs` (IPv6 normalization).

**Approach:** Make the format unambiguous for any path — the port terminates at the first non-digit, or the path is encoded so it always presents a leading `/`; pick whichever keeps existing absolute-path keys byte-identical, since those are persisted. Assert `parse(format(x)) == x` over generated fields including `~`, `~/x`, relative paths, spaces, metacharacters, and IPv6 forms. On read, repair a key in the old broken shape rather than rejecting it. Delete the raw-URI display-name fallback and the per-render `log::warn!` that prints the user's user, host, port, path, and identity path. Normalize a hand-typed `[::1]` to `::1` in the modal.

**Tests:** generated round-trip coverage is the deliverable. No property-test crate exists in this workspace (verified 2026-07-30), so the default shape is a generated input table over the documented grammar — the cross product of user, host form (name, IPv4, `::1`, `[::1]`), port, and path form (`/abs`, `~`, `~/x`, `rel/x`, empty, spaces, metacharacters) — asserted with `parse(format(x)) == x` for every row. Adding `proptest` as a test-only dev-dependency on `crates/repo_mode` is an acceptable substitute, not a prerequisite. Plus a fixed regression test for the exact `ssh://u@h:22~/projects/app` key, and a read-repair test for a key persisted in the broken shape.

### U6. Separate editor detection from editor launch

**Covers:** RC6, N13–N14. **Files:** `app/src/util/file/external_editor/{mod,mac,linux,windows}.rs` and their tests, `app/src/workspace/view/open_folder.rs` (telemetry).

**Approach:** Split the overloaded field into a detection probe and a launch target. `compute_installed_editors` reports an editor installed only when its launch target exists. Correct the Windows targets (`<InstallLocation>\Code.exe` rather than `bin\code.exe`; same for Windsurf) — verify each against a real install before landing, since the review could confirm the code path but not the shipped filenames. On Windows and Linux, a failed `spawn` falls through to `ctx.open_file_path` instead of returning past it, matching what macOS already does. Move the telemetry emit after the outcome is known and record what happened.

**If a Windows install cannot be verified, land the safe half.** The two changes have different risk profiles. The detection/fallback split is safe to land unverified: an editor whose launch target does not exist is reported not-installed and the folder is revealed in the file manager, which is strictly better than today's silent no-op regardless of which filename is right. Correcting a specific launch target is the part that needs a real install to confirm. If no Windows machine is available, land the split and the fallback, leave the existing target strings in place, and record the filename correction as an open item — do not guess a path and present it as verified. Same rule for the Linux desktop-entry targets.

**Tests:** NA8. A test per platform that an editor whose launch target is missing is not reported installed. A test that a failed launch emits `"finder"`.

### U7. One lazy, flag-gated, off-thread editor cache

**Covers:** RC7, N16. **Files:** `app/src/workspace/view.rs` (construction, `EditorSettings` subscription), `open_folder.rs` (`installed_editors_cached`), `app/src/settings_view/features/external_editor.rs`, `app/src/workspace/view/open_folder_tests.rs`.

**Approach:** Remove the synchronous sweep from `Workspace::new`; populate on first need and refresh in the background. Check the feature flag *before* rescanning in the `EditorSettings` subscription. Return a borrow or shared handle instead of cloning the `Vec<Editor>` per call. Collapse the Settings page's four independent sweeps into one per rebuild.

**Tests:** NA10. A test that the Settings page performs one detection pass rather than four.

### U8. A representable "no editor" state

**Covers:** RC8, N15. **Files:** `app/src/util/file/external_editor/settings.rs`, `app/src/settings_view/features/external_editor.rs`, `settings_tests.rs`.

**Approach:** Add the "None — reveal in file manager" row to the folder-editor dropdown. Return `None` from `resolve_default_folder_editor_with_installed` when the explicitly-chosen editor is not installed, so the documented reveal fallback is what actually happens. Persist the seed once as a first-run suggestion rather than recomputing `installed_editors.first()` on every read, so installing an application cannot silently change the user's default.

**Tests:** NA9. A test for each arm of `resolve_default_folder_editor_with_installed`, which currently has none. A test that installing a higher-priority editor does not change an already-persisted default.

### U9. One activation chokepoint

**Covers:** RC9, N6. **Files:** `app/src/workspace/view.rs` (`activate_tab_internal`, the `FocusPane` arm), `app/src/workspace/view_tests.rs`.

**Approach:** Call `sync_repo_mode_selection_to_active_tab` from `activate_tab_internal` and remove the `FocusPane` special case. Verify `remove_tab`'s post-close activation is covered by the same path.

**Tests:** NA3, driven through `ActivateNextTab` and `ActivateTabByNumber`, plus the close-tab fallback.

### U10. Sidebar affordances: destructive clicks and render-path work

**Covers:** N19, and the confirmed sidebar findings. **Files:** `app/src/workspace/view/repo_sidebar.rs`, `remote_connection_modal.rs` (`validate`), `repo_mode_model.rs` (badges), `app/src/workspace/view/repo_sidebar_tests.rs`, `app/src/workspace/view/remote_connection_modal_tests.rs`.

**Approach:** Make "Remove" its own hit target on a dead row — today the whole row is the click target and a single left-click deletes the registry entry with no confirmation, so a briefly-unavailable network mount turns a healthy repo into a one-click delete. Stop `validate()` calling `.exists()` from the render path (a stalled sshfs identity path freezes the UI on every keystroke); memoize on the identity editor's edit event. Add ellipsis clipping to the primary name and secondary path. Prune `entry_rows` / `pr_badges` / `branch_cache` when an entry leaves the registry, as the sibling caches already are. Cache `repo_mode_entry_badges`, currently O(entries × tabs × panes) per frame with no TTL.

**Tests:** NA11 — a dead row's non-Remove area does not dispatch removal, and the Remove target does. `validate` performs no filesystem access when the identity string is unchanged. Badge computation for N entries over M tabs is invoked once per frame rather than N×M times. A pruned entry leaves no residue in `entry_rows`, `pr_badges`, or `branch_cache`.

### U11. Residual defects

**Covers:** N20, and the remaining individually-confirmed findings with no shared cause. **Depends on:** nothing; lands last so it rebases over the other ten rather than under them.

**Land these as separate commits.** This unit is a collection, not one change: it holds a path-resolution bug, a security-message classification, a naming consistency fix, a flag gate, a product-visible behavior change (R12), and a Cargo edition bump. They share no cause and no test. Grouping them into one commit would make any one of them hard to revert on its own, which is the property every other unit in this plan has. One commit per bullet; the unit is the batch, not the atom.

**Files:** `app/src/workspace/view/startup_directory.rs` (+ a new `startup_directory_tests.rs`, which does not exist yet), `crates/repo_mode/src/entry.rs` and `entry_tests.rs` (`classify_probe_failure`), `app/src/workspace/view.rs` (session-restore backfill at the `display_name_for_path` call site, and the `selected_repo_root` restore assignment), `crates/repo_mode/Cargo.toml`.

- `startup_directory.rs`: the repo-root branch returns before `chosen_shell` / WSL / `same_system` are consulted, so opening a WSL tab with a repo selected hands a host path to a WSL shell. Take the branch only when the shell and the root are on the same system.
- Remote R12: when `warpify.ssh.enable_ssh_warpification` is off, no `WarpifiedRemote` session is created, so the tab connects and never `cd`s, silently. Surface a notice or fall back.
- `classify_probe_failure`: drop the ignored `_exit_code` parameter, and give "remote host identification has changed" its own reason — it currently maps to "connect once by hand first — the host key is unknown or the key is locked", which describes a first connection, not the canonical MITM signal. The new reason must read as a warning, not a setup instruction; telling a user to connect by hand is the wrong advice when the signal is a changed host key.
- Session restore: use `display_name_for_registry_path` rather than `display_name_for_path` for bound-group name backfill. Confirmed divergent — the restore-path backfill in `view.rs` calls `display_name_for_path`, while `repo_mode_model.rs`'s bound-group naming calls `display_name_for_registry_path`. Currently unreachable (the name is always set), but the two call sites should not disagree.
- Gate the `selected_repo_root` restore assignment on the flag, matching the pruned-group fallback below it.
- `crates/repo_mode/Cargo.toml` still declares `edition = "2021"` on a branch that reformatted for 2024.

**Tests:** NA12 — a WSL shell with a host repo root selected does not receive the host path verbatim; the same-system case still receives the repo root. `classify_probe_failure` maps a changed-host-key stderr to its own reason, distinct from the unknown-host-key reason, with one test per reason arm. The two `display_name_*` call sites agree for a registry path that differs between the two functions. With the flag off, restore assigns no `selected_repo_root`.

**Verification:** `cargo nextest run -p repo_mode` covers the codec and classifier arms; the `warp` lib tests cover startup-directory and restore. The `edition` bump is verified by `./script/presubmit` staying green — if it changes any behavior, that is a finding, not a formatting change.

**Note:** the R12 warpification item is the one bullet here that is a product-visible behavior change rather than a defect fix. If surfacing a notice needs copy that does not already exist, take the fallback arm instead and record the notice as an open item rather than inventing user-facing strings inside a remediation unit.

---

## Verification Contract

| Gate | Command / check | Applies to |
|---|---|---|
| Root-cause regression tests | Each unit ships at least one test that **fails on the pre-unit code**; verify by reverting the fix and running it | U1–U9 |
| Defect regression tests | Same standard, per confirmed defect rather than per root cause | U10, U11 |
| Unit tests | `cargo nextest run -p repo_mode` and the `warp` lib tests for `repo_mode`, `remote_connection_modal`, `open_folder`, `external_editor`, tab-bar slots | all |
| Generated round-trip coverage | `parse(format(x)) == x` over the generated grammar table (no property-test crate in this workspace — see Assumptions) | U5 |
| Presubmit | `./script/presubmit` (fmt, clippy, tests) | all |
| Flag-off parity | build with `repo_mode` and `open_folder_in_ide` removed from `default`; rendering and behavior identical to stock | every unit boundary |
| Security non-regression | the `ssh` line still quotes every field; the charset allowlist still rejects metacharacters | U4, U5 |
| Data-loss non-regression | last-tab close still confirms with `RepoMode` on; no single click removes a registry entry; no upgrade path deletes a persisted entry | U1, U3, U4, U9, U10 |
| Persisted-state reconciliation | a `projects` row written by the pre-U4 speculative path still renders, does not claim to be resolved, and does not log its connection string | U4, U5 |
| Manual smoke | horizontal tab bar with a filtered group; remote add cancelled mid-probe; remote re-add against a down host; Open folder with the default editor uninstalled; click a dead sidebar row; open a tab with a WSL shell and a host repo selected | U1, U4, U6, U8, U10, U11 |

## Definition of Done

- NA1–NA13 pass as automated tests, each verified to fail on the code before its unit.
- The four high-severity defects are gone, and the six remote-lifecycle findings are covered by tests against one state machine rather than five guards.
- Every unit U1–U11 traces to at least one requirement N1–N20, and every requirement is claimed by at least one unit.
- `./script/presubmit` green; flag-off parity verified at the last unit.
- No amendment in the three shipped plans still reads "resolution pending" or "pending decision".
- The deferred items in Scope Boundaries are recorded where a future reader will find them, not silently dropped.
- U2's changes are evaluated for an upstream PR and the decision recorded either way.

---

## Sources / Research

- Review of 2026-07-28: six parallel reviewers over `origin/master...vinh-main`, then three adversarial verifiers instructed to refute. Findings that survived refutation are the input to this plan; five claims were downgraded or refuted and are deliberately absent (menu double-mount, blocked by `prevent_interaction_with_other_elements`; the `id_rsa` group name, unreachable because the name is always set; the ungated `selected_repo_root` restore, self-healing via the sanity pass; `GroupedTabs` divergence, unreachable in any shipped build; and the "N clicks, N tabs" amplification, capped at one).
- Contiguity investigation: ~25 mutation sites audited; the invariant is documented at `view.rs:29592` as enforced and is not. One confirmed violator (`DragResult::ReorderInSource`), one plausible-unconfirmed (in-window drag leaving a group).
- Already shipped from the same review: `827b3a84a` (ssh destination quoting + charset allowlist), `f201d806e` (last-tab close confirmation), `ca6c6b1d9` (plan amendments).
