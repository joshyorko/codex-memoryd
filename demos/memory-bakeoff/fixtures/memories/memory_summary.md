# Memory Summary

- Decision: codex-memoryd stores durable memory in SQLite via rusqlite with the
  bundled feature, so no system libsqlite3 is required.
- Prefer repo-native commands: `cargo fmt --check` and `cargo test` validate the
  workspace.
- Gotcha: FTS5 is probed at startup; when unavailable the store falls back to
  LIKE search and reports `degraded` status.
