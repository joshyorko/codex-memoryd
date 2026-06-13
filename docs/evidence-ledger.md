# Evidence Ledger

The evidence ledger is an append-only provenance surface for memory writes. It
answers: what safe, scoped evidence caused the provider to accept, reject, or
promote a memory-related event?

The ledger is not a recall source and is not authority. Recall still reads
`memory_records`, `checkpoints`, and their existing policy-gated sources. The
ledger exists so operators and future agent adapters can audit write behavior
without reading raw rejected content or hidden implementation state.

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
