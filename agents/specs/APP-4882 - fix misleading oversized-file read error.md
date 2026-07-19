# Spec: Fix misleading "These files do not exist" error for oversized files in read_files

Linear: [APP-4882](https://linear.app/warpdotdev/issue/APP-4882/fix-misleading-these-files-do-not-exist-error-for-oversized-files-in)
Originating thread: https://warp-public.slack.com/archives/C0BDQDW8V5E/p1784468227182559
Estimate: M (3)
Commit-pinned references below are anchored at `warpdotdev/warp@69ce3728acae0b01c2f457b65a90c144664686aa`.

## PRODUCT

**Summary:** When the agent's `read_files` tool reads a local file that *exists*
but exceeds the per-file size cap (1 MB), the client returns
`These files do not exist: <path>`. That message is wrong — the file exists and
is readable, it is just too large. A real user hit this on a 3,476,751-byte JPEG
that failed repeatedly while an 851 KB downscaled copy of the same image
succeeded. The root cause is that a single failure variant
(`BinaryFileReadResult::Missing`) conflates five distinct failure reasons and
every consumer flattens them into one "do not exist" message. This change makes
each file-read failure carry its reason so the tool (and the other consumers of
the same code path) report an accurate, actionable message.

**Key design choices:**
1. **Carry a structured failure reason per file, end to end.** Split the
   catch-all `BinaryFileReadResult::Missing` into `NotFound`,
   `TooLarge { size_bytes, limit_bytes }`, and `ProcessingFailed { detail }`, and
   replace `ReadFileContextResult.missing_files: Vec<String>` with
   `failed_files: Vec<FileReadFailure>` (path + reason). This is the minimal
   change that lets every consumer emit an accurate message, and it mirrors the
   remote read path, which already surfaces per-file reasons via the proto
   `FailedFileRead.error.message`.
2. **Group the batch message by reason.** The three agent-tool consumers
   (`read_files`, `get_files`, `search_codebase`) build one error string that
   groups the failed files by reason — a "do not exist" group for genuinely
   missing files, a per-file "too large" line naming both sizes, and a
   "could not be processed" group — instead of a single flat list.
3. **Keep all-or-nothing batch semantics (partial-success is out of scope).**
   When any requested file fails, the local consumers still return an `Error`
   (they do not return the files that succeeded). Changing that is a separable
   behaviour change with its own blast radius; it is called out as a non-goal.

**Behavior** (numbered, testable invariants from the agent/consumer's view):
1. **Oversized existing file → accurate "too large" message (default repro).**
   Calling `read_files` on an existing, readable binary file whose size exceeds
   the per-file limit returns an `Error` whose message names the path and states
   the file is too large, including the file size and the limit — e.g.
   `<path> is too large to read (3.5 MB > 1.0 MB limit). Downscale/compress it or read a smaller copy.`
   It does **not** say the file does not exist.
2. **Genuinely missing file → unchanged "do not exist" message.** Calling
   `read_files` on a path that does not exist returns
   `These files do not exist: <path>` (multiple missing paths are comma-joined),
   preserving today's wording for the truly-not-found case.
3. **Image that fails processing → distinct "could not be processed" message.**
   When an image cannot be processed (resize/decode error, or still over the
   limit after processing), the message states the file could not be processed
   as an image and includes the reason detail — distinct from both "too large"
   and "do not exist".
4. **Mixed batch groups by reason.** A single `read_files` call that includes a
   missing file, an oversized file, and a processing-failure file returns one
   `Error` that reports all three, grouped by reason, each naming the relevant
   path(s) — no reason is dropped or mislabeled.
5. **Same accuracy across all agent-tool consumers.** `get_files` and
   `search_codebase` (which share `read_local_file_context`) emit the same
   reason-accurate, reason-grouped messages as `read_files` for the same inputs.
6. **Remote read path carries the reason too.** For a remote/host session, an
   oversized/missing/processing failure is reported through the proto
   `FailedFileRead.error.message` with the reason-specific text (not the generic
   "File not found or could not be read"), so the remote consumer surfaces the
   same accurate message.
7. **All existing successful reads are unaffected.** Text files, in-limit binary
   files, and images that process successfully are read and returned exactly as
   before; the byte/batch budgeting and truncation behaviour is unchanged.

**Non-goals:**
- **Partial-success batch semantics.** Returning the files that succeeded
  alongside the per-file failures (item #4 in the triage "Solution") is out of
  scope for this change; the local consumers keep today's all-or-nothing
  behaviour (any failure ⇒ `Error`). Called out as a deliberate scope boundary
  to bound blast radius; see *Design alternatives*.
- Changing the 1 MB per-file cap (`MAX_FILE_READ_BYTES`) or the image size/pixel
  limits.
- Any change to how successful files are read, truncated, or budgeted.

## TECH

**Context — how file reading and failures work today:**
- The per-file cap is `MAX_FILE_READ_BYTES = 1_000_000`
  ([`execute.rs:1078 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1078)).
- `read_local_file_context`
  ([`execute.rs:1102-1214 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1102-L1214))
  returns `ReadFileContextResult { file_contexts, missing_files: Vec<String> }`
  ([`execute.rs:1081-1089 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1081-L1089)).
  It pushes to `missing_files` in two places: on a metadata `NotFound`
  ([`execute.rs:1132-1136 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1132-L1136))
  and when the binary reader returns `Missing`
  ([`execute.rs:1209 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1209)).
- `BinaryFileReadResult`
  ([`execute.rs:1262-1270 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1262-L1270))
  has a single failure variant `Missing`, whose own doc comment admits the
  conflation ("File doesn't exist, exceeds the size limit, or couldn't be
  processed.").
- `read_binary_file_context`
  ([`execute.rs:1274-1322 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute.rs#L1274-L1322))
  returns `Missing` for **five** distinct failures:
  1. `file_size > max_bytes` (`execute.rs:1280-1282`) — the reported repro.
  2. `FileLoadError::DoesNotExist` (`execute.rs:1286`).
  3. `ProcessImageResult::TooLarge` (`execute.rs:1294-1296`).
  4. `ProcessImageResult::Error(err)` (`execute.rs:1298-1301`).
  5. processed content still `> max_bytes` (`execute.rs:1308-1310`).
- `ProcessImageResult` is defined at
  [`app/src/util/image.rs:107-117 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/util/image.rs#L107-L117)
  (`Success { data }`, `TooLarge`, `Error(ImageError)`); `ImageError` is
  `Display`.

**Consumers of `ReadFileContextResult` (all must be updated — one more than the
triage list):**
1. `read_files.rs` local path
   ([`read_files.rs:243-252 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute/read_files.rs#L243-L252)) —
   `ReadFilesResult::Error("These files do not exist: {missing_files}")`.
2. `get_files.rs`
   ([`get_files.rs:316-326 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute/get_files.rs#L316-L326)) —
   same pattern for `GetFilesResult`.
3. `search_codebase.rs`
   ([`search_codebase.rs:92-99 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute/search_codebase.rs#L92-L99)) —
   same "These files do not exist" message (`SearchCodebaseResult::Failed`).
4. `file_context_result_to_proto`
   ([`server_model.rs:3840-3885 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/remote_server/server_model.rs#L3840-L3885)) —
   maps `missing_files` to proto `FailedFileRead { path, error: "File not found or could not be read" }`.
5. **`passive_suggestions/legacy.rs`** (not in the triage list)
   ([`legacy.rs:440-449 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/passive_suggestions/legacy.rs#L440-L449)) —
   reads `content.missing_files` for a `log::warn!` and emits
   `PromptSuggestionFallbackReason::MissingFile`. It surfaces no user message, but
   it references the field so it **must** be updated to compile after the rename.

**The model to mirror:** the remote read path already reports per-file reasons.
`ReadFilesExecutor` remote branch reads `f.error.message` from each proto
`FailedFileRead`
([`read_files.rs:172-189 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/app/src/ai/blocklist/action_model/execute/read_files.rs#L172-L189)),
and the proto `FailedFileRead { path, FileOperationError { message } }` already
carries a per-file message
([`remote_server.proto:436-439 @ 69ce372`](https://github.com/warpdotdev/warp/blob/69ce3728acae0b01c2f457b65a90c144664686aa/crates/remote_server/proto/remote_server.proto#L436-L439)).
No proto change is needed — only `file_context_result_to_proto` must put the
reason-specific text into the existing `error.message`.

### Design alternatives

- **How to represent the per-file failure reason:**
  - *New `failed_files: Vec<FileReadFailure>` with a `reason` enum* — **chosen.**
    A dedicated struct `FileReadFailure { path: String, reason: FileReadFailureReason }`
    where `FileReadFailureReason` is `NotFound` / `TooLarge { size_bytes, limit_bytes }`
    / `ProcessingFailed { detail: String }`. Pros: structured, lets each consumer
    format its own message and group by reason; carries the exact sizes for the
    "too large" message; maps cleanly to the proto per-file `error.message`.
    Cons: touches the shared result type and all five consumers.
  - *Keep `Vec<String>` but embed the reason in the string* — rejected. It loses
    structure (consumers can't group by reason or reformat), forces the proto
    mapping to re-parse a human string, and couples message wording to the
    producer.
  - *Add a parallel `oversized_files: Vec<String>` next to `missing_files`* —
    rejected. It only special-cases "too large", still conflates not-found with
    processing failures, and multiplies fields as reasons grow.
- **Naming to avoid collision with the proto type:** the client-side struct is
  named `FileReadFailure` (reason enum `FileReadFailureReason`), distinct from
  the generated proto `remote_server::proto::FailedFileRead`, so the two never
  clash in `server_model.rs`.
- **Splitting `BinaryFileReadResult::Missing`:** replace the single `Missing`
  with `NotFound`, `TooLarge { size_bytes, limit_bytes }`, and
  `ProcessingFailed { detail: String }`, matched exhaustively (no `_` arm) so a
  future variant forces every call site to be revisited (per `AGENTS.md`). The
  two size failures (raw `file_size > max_bytes` and processed content still
  `> max_bytes`) both map to `TooLarge` with the respective observed size and the
  limit; `ProcessImageResult::TooLarge` also maps to `TooLarge` (its detail is
  the post-processing size vs. limit), while `ProcessImageResult::Error` maps to
  `ProcessingFailed`.
- **Batch semantics (partial success):** keep all-or-nothing (any failure ⇒
  `Error`) — **chosen** for this change. Returning successful files alongside
  failures (as the remote branch does at `read_files.rs:172`) is a real behaviour
  change to the tool contract with its own blast radius across the three
  consumers and the agent's expectations; it is deferred as a non-goal and left
  as a documented follow-up rather than folded into a messaging fix.
- **Message wording / size formatting:** the "too large" message includes both
  the file size and the limit rendered in MB with one decimal (e.g. `3.5 MB` /
  `1.0 MB`), via a small local helper (there is no existing human-byte formatter
  in `app/src` — confirmed by grep). Wording matches the ticket's acceptance
  criteria. A single message-building helper on the reason (or a free function)
  keeps the three tool consumers consistent and unit-testable in isolation.
- **Feature gating:** `FileReadFailure` / `FileReadFailureReason` and
  `ReadFileContextResult.failed_files` must **not** be `#[cfg(feature = "local_fs")]`,
  because `ReadFileContextResult` is used by `server_model.rs` regardless of the
  feature; only `BinaryFileReadResult` and the `read_*_file_context` bodies stay
  `local_fs`-gated as today.

### Proposed changes

1. **`app/src/ai/blocklist/action_model/execute.rs`**
   - Add (ungated) the failure types:
     ```rust
     #[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
     pub enum FileReadFailureReason {
         /// The file does not exist on disk.
         NotFound,
         /// The file exists but exceeds the per-file byte limit.
         TooLarge { size_bytes: usize, limit_bytes: usize },
         /// The file could not be processed (e.g. image decode/resize failed).
         ProcessingFailed { detail: String },
     }

     #[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
     pub struct FileReadFailure {
         pub path: String,
         pub reason: FileReadFailureReason,
     }
     ```
   - Give `FileReadFailureReason` (or `FileReadFailure`) a `message()` helper that
     renders the per-file human/model-readable text (used by the proto mapping and
     reusable by the tool consumers), plus a small `fn format_mb(bytes: usize) -> String`
     helper for the size rendering. Add a batch helper (e.g.
     `fn describe_failures(failures: &[FileReadFailure]) -> String`) that groups by
     reason: a single `These files do not exist: <comma-joined paths>` line for the
     `NotFound` group, one `... is too large to read (X MB > Y MB limit). ...` line
     per `TooLarge` file, and a
     `These files could not be processed as an image: <path>: <detail>` group for
     `ProcessingFailed`. This shared helper is what the three tool consumers call.
   - Replace `ReadFileContextResult.missing_files: Vec<String>` with
     `failed_files: Vec<FileReadFailure>`; fix the stale field doc comment
     (`execute.rs:1086-1088`).
   - Split `BinaryFileReadResult::Missing` into `NotFound`,
     `TooLarge { size_bytes, limit_bytes }`, `ProcessingFailed { detail }`; update
     `read_binary_file_context` to return the specific variant at each of the five
     sites (carrying `file_size`/processed len and `max_bytes`, or the
     `ImageError` detail); fix the `BinaryFileReadResult` doc comment
     (`execute.rs:1268`).
   - In `read_local_file_context`, push `FileReadFailure { path, reason: NotFound }`
     at the metadata-`NotFound` site (`execute.rs:1132-1136`) and map each
     `BinaryFileReadResult` failure variant to the corresponding
     `FileReadFailureReason` at `execute.rs:1209` via an exhaustive match. Fix the
     stale function doc comment (`execute.rs:1091-1100`) that claims oversized
     files are "reported as oversized".
2. **`read_files.rs`, `get_files.rs`, `search_codebase.rs`** — replace the
   `result.missing_files.is_empty()` / `join(", ")` blocks with
   `result.failed_files.is_empty()` and, when non-empty, build the error message
   via the shared `describe_failures(&result.failed_files)` helper (keeping each
   consumer's existing result type/wrapper and, for `search_codebase`, the
   existing `SearchCodebaseFailureReason::InvalidFilePaths`).
3. **`server_model.rs` `file_context_result_to_proto`** — map each
   `FileReadFailure` to proto `FailedFileRead { path: f.path, error: Some(FileOperationError { message: f.reason.message() }) }`
   so the remote consumer surfaces the reason-specific text (replaces the generic
   "File not found or could not be read").
4. **`passive_suggestions/legacy.rs`** — update the `content.missing_files`
   reference (`legacy.rs:440-444`) to `content.failed_files` (log the paths, and
   optionally the reasons); behaviour otherwise unchanged
   (`PromptSuggestionFallbackReason::MissingFile` still emitted).
5. **Conventions (`AGENTS.md`):** inline format args (`format!("{path}")`),
   exhaustive matches (no `_`), imports at the top, remove any unused params
   entirely (no `_` prefix), and no comments that merely restate code; only update
   comments whose logic changed (the three stale doc comments above).

**Open questions resolved:**
- *How to carry the reason?* New `failed_files: Vec<FileReadFailure>` with a
  `FileReadFailureReason` enum — resolved (see *Design alternatives*).
- *Name collision with proto `FailedFileRead`?* Use `FileReadFailure` client-side
  — resolved.
- *Proto change needed?* No — the existing per-file `error.message` carries the
  reason — resolved from the proto and the remote read path.
- *Exact "too large" wording / size units?* MB with one decimal via a local
  helper, matching the acceptance criteria — resolved (no existing byte-size
  humanizer in `app/src`).
- *How many consumers?* Five, not four — `passive_suggestions/legacy.rs` also
  reads the field and must be updated to compile — resolved by grep.
- *Feature gating of the new types?* Ungated (server_model uses the result type
  regardless of `local_fs`) — resolved.
- *Partial-success behaviour?* Explicitly out of scope (non-goal) — resolved.

**Risks / blast radius:**
- The change is confined to the file-read result type and its five consumers; no
  proto/schema change and no change to successful-read behaviour or the byte
  budgeting.
- The `ReadFileContextResult` field rename is a compile-time break that surfaces
  every consumer — the exhaustive match on `BinaryFileReadResult` and the field
  rename make it impossible to miss one (mitigation, not a risk).
- `ReadFileContextResult` derives `Serialize`/`Deserialize`; if it is persisted
  or sent across a version boundary anywhere, the field rename changes the
  serialized shape. Implementation must check for any serialized/persisted use of
  `missing_files` (grep) and confirm there is none before merge; the current
  consumers use it only in-process.
- Message wording is agent/model-facing; keep it concise and actionable so it
  does not bloat the tool result.

## Validation & verification criteria (must ALL pass before merge)

1. **Reproduction is fixed (regression test).** A new unit test creates a temp
   binary file larger than the per-file limit and asserts
   `read_local_file_context` returns it in `failed_files` with
   `FileReadFailureReason::TooLarge { .. }`, and that the message rendered for it
   contains the path, the file size, and the limit and does **not** contain "do
   not exist". This test fails against the current code (which returns a
   `missing_files` string and the "do not exist" message) and passes after the
   change. *Checked by: `cargo nextest run -p warp` (the affected test module);
   suggested location `app/src/ai/blocklist/action_model/execute_tests.rs`.*
   Triage recorded an environment-mismatch skip for a full hands-on repro
   (building/running the whole Warp client is impractical in the sandbox); this
   deterministic regression test is the reproduction contract per
   `factory-verification`.
2. **NotFound still says "do not exist".** A unit test asserts a genuinely
   non-existent path yields `FileReadFailureReason::NotFound` and a message of the
   form `These files do not exist: <path>`. *Checked by: `cargo nextest run -p warp`.*
3. **Processing failure is distinct.** A unit test drives the
   `ProcessingFailed`/image-error path (a `.png`/`.jpg`-named file with invalid
   image bytes under the size limit) and asserts the failure is
   `FileReadFailureReason::ProcessingFailed { .. }` with a message stating the
   image could not be processed (distinct from "too large" and "do not exist").
   *Checked by: `cargo nextest run -p warp`.*
4. **Mixed-batch grouping.** A unit test on the batch helper
   (`describe_failures`) with one of each reason asserts all three are present,
   grouped by reason, each naming the correct path(s), with no reason dropped or
   mislabeled (behavior #4). *Checked by: `cargo nextest run -p warp`.*
5. **All three tool consumers use the shared message.** Confirm `read_files`,
   `get_files`, and `search_codebase` build their error from
   `describe_failures(&result.failed_files)` (behavior #5). *Checked by: code
   review of the three call sites + the tests above; no consumer retains the
   literal `"These files do not exist: "` prefix except via the shared NotFound
   grouping.*
6. **Remote proto mapping carries the reason.** A unit test on
   `file_context_result_to_proto` asserts a `TooLarge`/`ProcessingFailed`
   `FileReadFailure` produces a proto `FailedFileRead` whose `error.message` is
   the reason-specific text (not the generic "File not found or could not be
   read") (behavior #6). *Checked by: `cargo nextest run -p warp`
   (`server_model_tests.rs`).*
7. **Successful reads unaffected.** Existing `execute`/read tests for text files,
   in-limit binaries, and successful images still pass unchanged (behavior #7).
   *Checked by: `cargo nextest run -p warp`.*
8. **No collateral damage / everything compiles.** All five consumers (including
   `passive_suggestions/legacy.rs`) compile with the renamed field; no `_`
   wildcard arm hides an unhandled `BinaryFileReadResult`/`FileReadFailureReason`
   variant. *Checked by: `./script/presubmit` (build + clippy) with no new
   warnings.*
9. **Presubmit passes.** `./script/presubmit` is green — `./script/format`,
   `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings`,
   build, and tests. *Checked by: running it.*

## Notes for implementation
- This change is **not** a distinct rendered UI surface: the affected output is
  the `read_files`/`get_files`/`search_codebase` **tool-result message text**
  (consumed by the agent/model and shown verbatim in the tool-call result). Its
  correctness is fully and deterministically captured by the unit tests above on
  the exact strings, so no `computer_use`/screenshot proof is required (matching
  triage's verification plan and `factory-verification`'s deterministic-check
  mandate for non-UI-rendering changes).
- Place new unit tests in the existing `execute_tests.rs` (for
  `read_local_file_context`/`read_binary_file_context`, which already exercise
  `local_fs`-gated helpers) and `server_model_tests.rs` (for the proto mapping),
  following the repo's `${filename}_tests.rs` convention.
- Keep imports at the top of each module and prefer inline format args per
  `AGENTS.md`.
