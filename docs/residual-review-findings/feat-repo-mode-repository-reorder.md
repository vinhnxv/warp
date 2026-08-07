# Residual review findings — `feat/repo-mode-repository-reorder`

Branch: `feat/repo-mode-repository-reorder` (base `vinh-main`)
Change: repo-mode repository reorder (plan `docs/plans/2026-08-06-001-feat-repo-mode-repository-reorder-plan.md`)
Review run: `20260807-025406-3401ec46`

Reviewers that returned: correctness, security, performance, project-standards, data-migration,
adversarial. Three more — testing, reliability, maintainability — were dispatched and never
returned; see Coverage gaps below.

Seven findings were applied in `1803d7f1d`, one of them only partially. Four more (R1, R2, R3, R6)
were first routed here as residuals and then fixed in a later pass, once the design questions two of
them carried had been settled with the user; their reasoning is kept below because it explains the
shape each fix took. R8 was found in manual testing after the review and is also fixed.

What remains genuinely open is R4, R5 and R7. This file records all of it — applied and not — with
the reasoning, so the decisions do not have to be re-derived.

No tickets were filed. This repository's issue tracker belongs to the upstream OSS project, and
filing there for an unmerged personal branch was not authorized.

---

## Fixed after the review, in a later pass

These three were routed here as residuals, then taken up and fixed. The reasoning is kept because it
explains why each fix takes the shape it does.

### R1. Reset order could be silently undone by another window — **fixed**

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

The user chose between the two available readings: the second window keeps rendering what it is
showing, so KTD6 stands, but its pin is treated as stale and its next drop rebuilds from the
registry instead of replaying the pin.

Fixed without adding any cross-window channel, because the second window can detect the reset
locally. It records whether it has ever rendered while a non-empty stored order existed; if it then
finds the stored order empty at drop time, that is somebody else's reset rather than the
pre-first-drag state. It drops its pin, writes nothing, and re-sorts by recency on the next render.
The drag that landed on the stale list is discarded once. The window that performed the reset clears
the same flag, so its own next drag takes the ordinary first-drag path.

Known limit of the signal: removing every repository that carried a manual position produces the
same evidence as a reset, so one drag in a window that had rendered with that order would be
discarded. It self-heals after that one gesture.

### R2. Scrolling mid-drag was read as the tab-block collapse — **fixed**

`app/src/workspace/view/repo_mode_model.rs` — from `adversarial`, P2, confidence 75.

The drag corrects for the selected repository's tab block collapsing by measuring how far the
dragged row's slot moved between frames. A scroll moves every slot by the same mechanism, so a
mid-drag scroll was absorbed as a collapse correction and the dragged rect read at the wrong offset
for the rest of the gesture.

Fixed by measuring the scroll off a row the collapse provably cannot move. The obvious candidate —
the dragged row's neighbour, which the drag already holds — is wrong: two adjacent rows are almost
always on the same side of the tab block, so a neighbour moves with the collapse exactly as the
dragged row does, and subtracting it would cancel the correction the anchor exists to make. The
list's *first* row works instead: `render_repo_tree` appends rows to one column in list order and
inserts the tab block immediately after the selected repository's own row, so nothing at or above
that row shifts when the block folds, and the first row is never below it. Whatever the first row
does is therefore scrolling, and taking it back out leaves the collapse alone.

Boundaries, deliberate: when the dragged row *is* the first row the correction is identically zero,
which is correct, since a first row is never below the block; and with no reference rect resolvable
the drag behaves exactly as it did before.

### R3. `freeze_anchor` lost the correction when a swap preceded the collapse render — **fixed**

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

Fixed the way that paragraph describes. A swap moves the slot by a knowable amount — the distance to
the row it exchanges slots with, in a known direction — so `freeze_anchor` became `record_swap`,
which accumulates that expected displacement instead of settling the anchor. What remains after
subtracting it, and after subtracting the scroll from R2, is the collapse. The rejected no-op
variant was re-checked and still breaks
`test_a_slot_move_after_a_swap_is_not_read_as_the_tab_block_collapse`, which continues to pass under
the fix that shipped.

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

### R6. A resolving remote row jumped from top to bottom — **fixed**

`app/src/workspace/view/repo_mode_model.rs` — noticed while applying the pin unverified-first fix.

A "Connecting…" row sorts to the top of the list. The moment the key verified it stopped being
unverified and fell back to its pin position — the appended slot, i.e. last — so the row jumped the
full length of the list at connect time. Consistent with R4 in the plan (a newly added repository
appends), but guaranteed rather than incidental, and on a long list it read as the row disappearing.

Fixed by remembering which keys this window put on screen as connecting and joining those to the
session pin at the *front* rather than the end, once. Every other newly appearing key still appends,
which is R4 unchanged. `commit_remote_key` also rewrites the pin in place when the host expands a
key into a different one, so the row the user watched connect keeps its slot instead of being
retired from the pin and re-appended under its resolved name.

### R7. `repo_row_position_id` interpolates the full registry key

`app/src/workspace/view/repo_sidebar.rs` — noticed while applying the log-leak fix.

The `SavePosition` id is `format!("repo_mode:row:{}", path.to_string_lossy())`. Position ids are
not logged today — `element_position_by_id` only reaches the position cache — so this is not a live
leak. It is the same class as the fixed finding, and would become one if element ids ever reached a
diagnostic dump.

---

## Found after review, in manual testing — fixed

### R8. A registered remote row was not draggable until clicked once per app launch — **fixed**

`app/src/workspace/view/repo_sidebar.rs` (`repo_row_is_draggable`). Reported by the user while
dragging; confirmed by tracing the probe lifecycle. This was a defect in this branch's feature, and
it is recorded here because the reasoning is worth keeping, not because it is outstanding.

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

Fixed by carrying `unverified` onto `RepoModeListEntry` from the set `repo_mode_entries` already
computes, and gating on that instead of on `probe`. A registered entry now drags immediately even
while `Pending`, and a genuinely in-flight first probe stays blocked exactly as R14 intends. The two
top-of-list sort exceptions read the same field, so there is now one definition of "verified"
instead of three lookups against the same set.

The regression is pinned: `an_unverified_remote_row_is_the_only_row_that_cannot_be_dragged` fails on
the registered-and-pending assertion if the gate is put back on `probe`, which was confirmed by
reverting it.

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
