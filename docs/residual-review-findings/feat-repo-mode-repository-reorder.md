# Residual review findings — `feat/repo-mode-repository-reorder`

Branch: `feat/repo-mode-repository-reorder` (base `vinh-main`)
Change: repo-mode repository reorder (plan `docs/plans/2026-08-06-001-feat-repo-mode-repository-reorder-plan.md`)
Review run: `20260807-025406-3401ec46`

Reviewers that returned: correctness, security, performance, project-standards, data-migration,
adversarial. Three more — testing, reliability, maintainability — were dispatched and never
returned; see Coverage gaps below.

Seven findings were applied in `0b88b23b5`, one of them only partially, and one more was applied
experimentally and rejected (R3 below). This file records everything that was not applied, with the
reasoning, so the decisions do not have to be re-derived.

No tickets were filed. This repository's issue tracker belongs to the upstream OSS project, and
filing there for an unmerged personal branch was not authorized.

---

## Not applied — needs a decision

### R1. Reset order does not reach another window's session pin, and can be silently undone

`app/src/workspace/view/repo_mode_model.rs` — from `adversarial`, P1, confidence 75.
`autofix_class: manual`, owner `human`.

Reset order clears the persisted `manual_position` on every row. It does not clear
`Workspace::repo_mode_launch_order`, the per-window session pin. A second window that has already
drawn its Repositories list keeps its pin and keeps rendering the old order — which is what KTD6
settles deliberately, so that half is by design.

What KTD6 did not consider is the write-back. The next drag in that second window takes the
`stored.is_empty()` branch and hands its whole pre-reset pin to `set_manual_order`, which persists
it. The reset is then undone, and the first window picks the old order back up. The user's escape
hatch quietly fails.

Fixing this means deciding what reset means across windows: broadcast the clear to every window's
pin, or mark a pin as reset-invalidated so the next drag rebuilds it from the registry instead of
replaying it. Both change settled KTD6 semantics, so this is a design call, not a mechanical fix.

### R2. Scrolling mid-drag is read as the tab-block collapse

`app/src/workspace/view/repo_mode_model.rs` — from `adversarial`, P2, confidence 75.
`autofix_class: manual`.

The drag corrects for the selected repository's tab block collapsing by measuring how far the
dragged row's slot moved between frames. A scroll during the drag moves every slot by the same
mechanism, and the drag cannot tell the two apart, so a mid-drag scroll is absorbed as a collapse
correction and the dragged rect reads at the wrong offset for the rest of the gesture.

Distinguishing them needs the scrollable's offset at each frame, which the drag does not currently
receive. Not attempted here.

### R3. `freeze_anchor` still loses the correction when a swap precedes the collapse render

`app/src/workspace/view/repo_mode_model.rs` — from `adversarial`, P2, confidence 75.
Reviewer's proposed fix was **rejected**; the underlying gap is real and stands.

If a swap is decided on a frame delivered before the collapse has been painted, the anchor freezes
at zero and the dragged rect sits a tab-block height low for the rest of the drag. The reviewer
proposed making `freeze_anchor` a no-op while `anchor_delta` is `None`. That was applied
experimentally and rejected: when nothing is folded away — the ordinary case, no repository
selected — the measured delta is legitimately zero and never stored, so the proposed guard makes
`freeze_anchor` unconditionally dead and lets the *post-swap* slot move be captured as a collapse,
producing one swap per frame. `test_a_slot_move_after_a_swap_is_not_read_as_the_tab_block_collapse`
now pins that behavior and fails under the proposed change.

A real fix requires the drag to tell swap-induced slot movement from frame-shift movement, rather
than inferring the collapse from raw slot delta.

---

## Not applied — out of scope for this change

### R4. A pre-existing security finding — deliberately not described here

From `security`, P2, confidence 75, `pre_existing: true`.

The security reviewer raised one finding that predates this branch and affects code that already
ships. It is not fixed here: it spans sixteen call sites across four files and needs its own change,
and folding it into a feature branch would bury it in unrelated diff.

**It is deliberately not described in this file.** This repository is a public fork of
`warpdotdev/warp`, `AGENTS.md` forbids disclosing a non-public security issue through a public pull
request or branch update, and `SECURITY.md` routes unpatched issues to security@warp.dev or a
private GitHub advisory instead.

Report it through that channel. Note that the reviewer's `security.json` artifact lives under
`/tmp`, which does not survive indefinitely — do not rely on it as the record.

---

## Not applied — advisory

### R5. Two performance advisories, both bounded by registry size

`app/src/workspace/view/repo_mode_model.rs` — from `performance`, P3, confidence 50.
`autofix_class: advisory`.

The drag path rebuilds the entry list and re-sorts on each frame, and `set_manual_order` walks
every ordered path on each drop. Both are O(repositories) with a small constant on a list a person
scrolls by hand. Recorded, not acted on; revisit only if a registry large enough to matter shows up.

### R6. A resolving remote row visibly jumps from top to bottom

`app/src/workspace/view/repo_mode_model.rs` — noticed while applying the pin unverified-first fix.

That fix makes a "Connecting…" row sort to the top of the list as intended. The moment the key
verifies it stops being unverified and falls back to its pin position — the appended slot, i.e.
last. The row therefore jumps the full length of the list at connect time. This is consistent with
R4 in the plan (a newly added repository appends), but the jump is now guaranteed rather than
incidental, and on a long list it may read as the row disappearing.

### R7. `repo_row_position_id` interpolates the full registry key

`app/src/workspace/view/repo_sidebar.rs` — noticed while applying the log-leak fix.

The `SavePosition` id is `format!("repo_mode:row:{}", path.to_string_lossy())`. Position ids are
not logged today — `element_position_by_id` only reaches the position cache — so this is not a live
leak. It is the same class as the fixed finding, and would become one if element ids ever reached a
diagnostic dump.

---

## Found after review, in manual testing — not fixed

### R8. A registered remote row is not draggable until it is clicked once per app launch

`app/src/workspace/view/repo_sidebar.rs:124` (`repo_row_is_draggable`). Reported by the user while
dragging; confirmed by tracing the probe lifecycle. **This is a defect in this branch's feature.**

R14 blocks a drag on `RemoteProbeState::Pending` because a remote key whose first probe has not
resolved is still provisional — the host can expand the path into a different key, so an order
written against it would name a repository that is about to stop existing. That reasoning holds for
an entry being added right now. It does not hold for an entry that is already in the registry with
a settled, persisted key and merely has not been probed in this window yet.

Both states are the same value. `repo_mode_remote_probes` (`app/src/workspace/view.rs:1269`) is
runtime-only and per-window, empty at startup, with no background poll, no startup probe, and no
retry — so after every app launch every registered remote row reads `Pending`
(`remote_list_entry`, `repo_mode_model.rs:1683`, `.unwrap_or_default()`) and is therefore
undraggable. The only way out is selecting the row (`repo_mode_model.rs:979-983`), and selecting a
remote entry that has no tabs also force-opens one (`repo_mode_model.rs:997-1021`). That is why the
symptom presents as "an empty remote repository cannot be dragged": the tab count is a correlate,
not the cause. Local rows are unaffected — `entry.remote` is `None`, so the gate never applies.

The distinction the gate needs already exists but does not reach it: `repo_mode_entries:170-176`
computes `unverified` as the probe keys absent from the registry, since the registry is what "verified"
means (R9). `RepoModeListEntry` (`repo_mode_model.rs:64-76`) does not carry that flag, so
`repo_row_is_draggable` cannot see it.

Fix shape: carry `unverified` onto `RepoModeListEntry` from the set already computed at line 170,
and gate on that instead of on `probe`. A registered entry then drags immediately even while
`Pending`, and a genuinely in-flight first probe stays blocked exactly as R14 intends.

---

## Coverage gaps

**Three reviewers never returned.** `testing`, `reliability`, and `maintainability` were dispatched
and failed — stream stalls at the 600s ceiling and connections closed mid-response, under nine-way
concurrency. Re-dispatch in batches of three recovered the other six; these three were not
successfully re-run. Their lenses are therefore **unreviewed**: test adequacy and coverage shape,
error/retry/timeout handling in the drag and persistence paths, and structural maintainability of
the new drag state machine. The correctness and adversarial reviewers touched adjacent ground, but
none of the three lenses was covered directly.

**No cross-model adversarial pass ran.** No peer CLI is installed on this machine (`codex`, `grok`,
`cursor-agent`, `composer` all absent) and no `cross_model_peer:` route is configured, so the
independent cross-model pass could not start. The in-process `adversarial-reviewer` covered the
lens as the documented fallback. This is route unavailability, not a policy decision — and no Warp
source left the machine.
