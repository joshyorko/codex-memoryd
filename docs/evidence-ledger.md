# Evidence Ledger

The evidence ledger is an append-only provenance surface for memory writes. It
answers: what safe, scoped evidence caused the provider to accept, reject, or
promote a memory-related event?

The ledger is not a recall source and is not authority. Recall still reads
`memory_records`, `checkpoints`, and their existing policy-gated sources. The
ledger exists so operators and future agent adapters can audit write behavior
without reading raw rejected content or hidden implementation state.

When read surfaces need to point back at ledger-backed evidence, they must emit
opaque `msrc_*` handles instead of raw `source_id` or `source_path` values.
Those handles are inert references, not bearer tokens or dereference authority.

## Row Model

Each row is scoped by:

- `profile_id`
- `workspace_id`
- optional `repo_id`
- optional `subject_key`

Each row records source metadata:

- `source_kind`: visible turn, conclusion, checkpoint, sync import, or Dreamer
  apply
- `source_id`: durable source/entity id when one exists
- `source_path`: stable synthetic path such as `turn:<id>` or local import path
- `source_hash`: deterministic hash for source identity and dedupe

Each row also stores:

- `safe_summary`: normalized, capped text intended for audit display
- `policy_state`: `accepted`, `secret_detected`, `policy_denied`,
  `invalid_request`, or another stable policy code
- `metadata`: bounded JSON with source counts, ids, labels, and reasons

## Safety Rules

Ledger writes must not store:

- raw rejected secret content
- `.env` dumps
- private keys or auth files
- encrypted or hidden reasoning
- giant raw logs

Rejected content may contribute to a hash used for idempotency, but the raw
content must not appear in `safe_summary`, `metadata`, or `source_path`.

Accepted summaries are still screened by the same write policy before the
provider stores them. They are capped and normalized so they are useful for
debugging without becoming a second unbounded content store.

## Multimodal Evidence

Sync imports can reference evidence that is not plain local-memory markdown:
screenshots/images, OCR extracts, log excerpts, document excerpts, git diffs,
and terminal output excerpts. The daemon treats `content` for these kinds as an
already-extracted text excerpt, not as a raw image/blob/document payload.

Raw artifacts are not stored in SQLite by default. The source path, caller hash,
and allowlisted metadata such as `artifact_ref` and `media_type` preserve the
artifact reference for audit and dedupe. The metadata also records
`raw_artifact_stored: false` and a `redaction_state`.

Secret-like material in extracted multimodal text is redacted before durable
record storage. The redaction marker names the detection class, not the matched
secret bytes. Prompt-injection text is still rejected rather than imported.

## Mutation Rules

`sync-local --preview` does not write ledger rows. Apply-mode sync writes one
idempotent row per imported source, plus rejected rows for files that apply mode
refuses to import.

Visible turns, conclusions, and checkpoints write ledger rows through the
service layer after profile/workspace/repo resolution and policy screening.

Dreamer writes rows only in apply mode. Preview reports remain non-mutating.

## Dedupe Rules

Rows use an internal `event_key` so repeated apply operations do not duplicate
the same evidence. The key includes scope, source identity, source hash, and
policy state. Rejected items use a content digest inside the source hash so two
different rejected inputs with the same policy code are not collapsed.
