# Integration fixtures

JSON request fixtures for the codex-memoryd `/v1` API, named
`<endpoint>.request.json`. They capture the exact wire shape the Codex-side
`codex_memoryd` provider client must produce (see
[`../../docs/codex-integration.md`](../../docs/codex-integration.md)).

Both sides can use these as a shared contract:

- **codex-memoryd** validates that each request fixture deserializes into its
  protocol request type and that a representative request produces the expected
  response — see [`../contract_fixtures.rs`](../contract_fixtures.rs).
- **Codex (PR #55 follow-up)** can serialize its `LocalCodexMemorySyncRequest`,
  recall, and turn payloads and assert they match these fixtures byte-for-shape,
  guaranteeing the two implementations stay aligned.

| Fixture | Endpoint | Maps to protocol type |
| --- | --- | --- |
| `status.response.json` | `GET /v1/status` (sample data) | `StatusResponse` |
| `recall.request.json` | `POST /v1/recall` | `RecallRequest` |
| `turns.request.json` | `POST /v1/turns` | `TurnsRequest` |
| `sync_local.preview.request.json` | `POST /v1/sync/local-codex-memory` | `SyncRequest` |
| `sync_local.apply.request.json` | `POST /v1/sync/local-codex-memory` | `SyncRequest` |

All request fields are optional / defaulted server-side; these fixtures show the
fields the Codex runtime actually sends. The daemon infers `kind` from `path`
and computes its own `source_hash`, so `hash`/`kind` on sync files are optional.
