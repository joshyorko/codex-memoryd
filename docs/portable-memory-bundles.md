# Portable memory bundles

`codex-memoryd.bundle.v1` is an offline, selective transfer format for reviewed
memory. A bundle is inert data: it does not contain a SQLite database, a
filesystem root, credentials, executable content, or a destination authority.
The operator must inspect and preview a bundle for the destination before
applying it.

## Workflow

On the source instance, export either a bounded preview or one new owner-only
artifact:

```text
codex-memoryd --local bundle export --preview \
  --profile personal --workspace codex-memoryd --target-profile personal
codex-memoryd --local bundle export --write ./reviewed.cmembundle \
  --profile personal --workspace codex-memoryd --target-profile personal
```

Inspection is database-free and may be performed anywhere:

```text
codex-memoryd bundle inspect ./reviewed.cmembundle
```

On the destination, preview performs no durable writes. Apply requires the
`plan_id` returned by the preview and recomputes the plan inside an immediate
SQLite transaction:

```text
codex-memoryd --local bundle import --preview ./reviewed.cmembundle \
  --to-profile personal --to-workspace codex-memoryd
codex-memoryd --local bundle import --apply ./reviewed.cmembundle \
  --to-profile personal --to-workspace codex-memoryd --plan-id sha256:...
```

Bundle export/import are local/admin-only in v1. `bundle inspect` does not
open configuration or a memory database. Existing `export`, `backup`, and
`import chatgpt-export` commands retain their existing meanings.

## Artifact contract

The transport is a ZIP file with these fixed members:

* `manifest.json`
* `objects/subjects.jsonl`
* `objects/episodes.jsonl`
* `objects/sources.jsonl`
* `objects/evidence.jsonl`
* `objects/memories.jsonl`
* optional `signatures/manifest.dsse.json`

ZIP bytes, entry order, timestamps, and compression choices are not identity.
Every payload member is declared by the canonical manifest and has a SHA-256
descriptor over its exact JSONL bytes. Every object envelope has a SHA-256
digest over the RFC 8785 JSON Canonicalization Scheme bytes of its closed
typed body. `bundle_id` is the SHA-256 digest of the unsigned canonical
manifest with `bundle_id` removed.

The reader validates names, duplicate entries, regular-file metadata,
encryption, compression, size/count/depth limits, canonical JSON, duplicate
JSON keys, object ordering, references, descriptors, and all digests before
planning or writing. It never extracts an archive to disk. Diagnostics contain
bounded reason codes and opaque references, not memory content.

The initial limits are intentionally for reviewed learnings rather than
conversation archives: 1 MiB manifest, 64 MiB per JSONL member, 128 MiB total
uncompressed payload, 20,000 objects, 5,000 root memories, 32 JSON nesting
levels, and bounded aliases, references, tags, files, and report details.

## Selection and continuity

Memory records are the only selection roots. Safe subjects, episodes, sources,
and directly linked accepted evidence are included when policy permits. Unsafe
or unavailable dependencies become bounded inert external references, or are
omitted with a visible count. Derived views, sessions, turns, procedures,
relations, FTS state, policy-event IDs, runtime configuration, and raw
artifacts are not payloads.

Portable references contain a public source instance identity, object kind, and
opaque handle (`mr_`, `msrc_`, `msub_`, `mep_`, or `mev_`). They are identity
labels only and never authorize lookup. Destination origin mappings preserve an
unchanged imported reference across re-export. A locally modified object
instead receives the destination instance identity while its prior origin
remains provenance.

Import has no semantic merge. It creates new validated objects, reuses an exact
origin or destination content match, or blocks the whole plan on an origin
revision, subject-key, incompatible-content, or destination-policy conflict.
There is no timestamp winner, fuzzy dedupe, overwrite, rename, or LLM
reconciliation. A successful apply writes the graph, origin mappings, and
durable receipt in one transaction. Repeating an applied bundle returns the
original receipt without creating rows.

## Security and handling

Integrity verification does not establish authenticity. Unsigned bundles report
`integrity=verified` and `authenticity=unverified`; the reserved DSSE member is
reported as present but unverified until an explicit trust policy exists.
Bundles are not encrypted. Treat them as containing personal or
work-confidential reviewed memory, use an appropriate transfer channel, keep
permissions owner-only, and securely delete temporary copies when no longer
needed. Do not commit bundle artifacts to a repository.
