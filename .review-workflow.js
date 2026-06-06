export const meta = {
  name: 'codex-memoryd-conformance-review',
  description: 'Adversarial SPEC-conformance audit of codex-memoryd: review dimensions, then verify each finding before reporting',
  phases: [
    { title: 'Review', detail: 'one reviewer per SPEC dimension, reads real code' },
    { title: 'Verify', detail: 'adversarially confirm each finding against the code' },
  ],
}

const ROOT = '/var/home/kdlocpanda/second_brain/Resources/codex-memory-lab/codex-memoryd'

const FINDINGS_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['findings'],
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        additionalProperties: false,
        required: ['title', 'severity', 'file', 'detail', 'spec_ref'],
        properties: {
          title: { type: 'string', description: 'one-line summary' },
          severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low'] },
          file: { type: 'string', description: 'path:line of the problem, or "none"' },
          detail: { type: 'string', description: 'what is wrong and why it violates SPEC/correctness' },
          spec_ref: { type: 'string', description: 'SPEC section or definition-of-done item' },
          suggested_fix: { type: 'string', description: 'concrete fix, or empty' },
        },
      },
    },
  },
}

const VERDICT_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  required: ['is_real', 'confidence', 'reason'],
  properties: {
    is_real: { type: 'boolean', description: 'true only if the finding is a genuine defect confirmed in the code' },
    confidence: { type: 'string', enum: ['high', 'medium', 'low'] },
    reason: { type: 'string', description: 'evidence from the code that confirms or refutes the finding' },
    corrected_severity: { type: 'string', enum: ['critical', 'high', 'medium', 'low', 'not-a-bug'] },
  },
}

const DIMENSIONS = [
  {
    key: 'endpoints',
    prompt: `You are auditing the codex-memoryd Rust provider for HTTP API conformance to its SPEC.
Read ${ROOT}/SPEC.md sections 5-6, then read ${ROOT}/src/protocol.rs, ${ROOT}/src/server.rs, and ${ROOT}/src/service.rs.
Verify EACH endpoint (GET /v1/status, POST /v1/recall, /v1/search, /v1/turns, /v1/conclusions, /v1/checkpoints, /v1/sync/local-codex-memory, /v1/forget, GET /v1/export) exists and that its request/response fields match the SPEC. Verify the common envelope (ok/data/error/warnings/request_id/provider) is correct. Flag missing endpoints, missing/renamed fields, wrong HTTP status codes, or responses that don't match SPEC. Report ONLY real discrepancies.`,
  },
  {
    key: 'secrets',
    prompt: `You are auditing codex-memoryd secret + prompt-injection detection.
Read ${ROOT}/SPEC.md section 10.1-10.2, then read ${ROOT}/src/policy.rs fully.
The SPEC requires blocking: private keys, API keys, passwords, auth tokens, .env dumps, credential files, raw secret manager output, encrypted reasoning, large raw logs likely to contain secrets, and prompt-injection-like durable memories.
Find concrete bypasses: a secret shape that SPEC requires blocking but the regexes miss, or an over-broad pattern that would reject ordinary prose (false positive). Test your hypotheses against the actual regexes in the file. Report only real gaps or real false-positive risks with a concrete example string.`,
  },
  {
    key: 'idempotency',
    prompt: `You are auditing codex-memoryd import idempotency and dedupe.
Read ${ROOT}/SPEC.md sections 7.9 and 4.2, then read ${ROOT}/src/ingest.rs, ${ROOT}/src/ids.rs, ${ROOT}/src/store.rs (focus on upsert_record, upsert_source, find_source, content_hash/source_hash usage).
The SPEC requires: apply is idempotent; re-importing unchanged files creates no duplicate records; dedupe tracks source_path/source_hash/content_hash/profile/workspace/repo_id. Trace the apply path and find any case where a duplicate record COULD be created, where the unique index could be violated and crash, or where a changed file fails to update. Report only real defects with the exact code path.`,
  },
  {
    key: 'boundaries',
    prompt: `You are auditing codex-memoryd profile-boundary and export safety.
Read ${ROOT}/SPEC.md sections 10.3 and 6.8, then read ${ROOT}/src/policy.rs (export_boundary, is_generic_preference) and ${ROOT}/src/export.rs.
Required matrix: work->personal deny; personal->work allow ONLY generic user operating preferences after classification; work->work allow; personal->personal allow; oss/homelab->personal implementation-defined. Export MUST omit secret_blocked and SHOULD include provenance. Find any path where work memory could leak to personal, where secret_blocked could be exported, or where the matrix is implemented incorrectly. Report only real leaks/violations.`,
  },
  {
    key: 'placeholders',
    prompt: `You are hunting for FAKE or placeholder implementations in codex-memoryd. The build must be a real provider, not scaffolding.
Read these files fully: ${ROOT}/src/recall.rs, ${ROOT}/src/service.rs, ${ROOT}/src/ingest.rs, ${ROOT}/src/store.rs, ${ROOT}/src/status.rs, ${ROOT}/src/server.rs.
Find: endpoints that return static/hardcoded responses regardless of input; functions that ignore their arguments; TODO/unimplemented!/todo!/panic! in non-test code paths; recall or search that doesn't actually query the store; counters/status fields that are always zero when they shouldn't be; dead code that fakes a feature. Report only genuine fakery, not stylistic issues. (Note: pending_writes=0 is legitimate because writes are synchronous.)`,
  },
  {
    key: 'recall',
    prompt: `You are auditing codex-memoryd recall correctness and the "memory is recall, not authority" contract.
Read ${ROOT}/SPEC.md sections 6.2, 8.1-8.4, 10.4, then read ${ROOT}/src/recall.rs and the recall path in ${ROOT}/src/service.rs and ${ROOT}/src/store.rs.
Verify: recall filters by profile AND workspace (no leakage); archived records never returned by default; secret_blocked never returned; ranking considers repo match, related-file match, type weight, recency, confidence; max_tokens budget respected; output marked as recall not authority. Find real correctness bugs: e.g. a filter that doesn't apply, a ranking signal that's inverted, a budget that's ignored, cross-workspace leakage. Report only real bugs.`,
  },
  {
    key: 'storage',
    prompt: `You are auditing codex-memoryd storage schema and migration correctness.
Read ${ROOT}/SPEC.md section 4.1.7 (memory record fields) and the task's required tables list (profiles, workspaces, repos, sessions, visible_turns, memory_sources, memory_records, conclusions, checkpoints, sync_cursors, policy_events). Then read ${ROOT}/migrations/0001_init.sql, ${ROOT}/migrations/0002_fts.sql, and the row mappers + migrate() in ${ROOT}/src/store.rs.
Verify: all 11 tables exist; memory_records has every required field (id, profile_id, workspace_id, repo_id, scope, type, content, related_files, tags, sensitivity, portability, confidence, source_ids, content_hash, supersedes, created_at, updated_at, last_used_at, archived, metadata); FTS5 triggers keep the index in sync on insert/update/delete; row mappers read the same columns the schema defines (column-order mismatches are critical). Report only real schema/mapper defects.`,
  },
  {
    key: 'tests',
    prompt: `You are auditing codex-memoryd test coverage against the SPEC's required test list.
Read ${ROOT}/SPEC.md section 15.3 and the task's required test list, then read ${ROOT}/tests/conformance.rs, ${ROOT}/tests/http_smoke.rs, ${ROOT}/tests/cli_smoke.rs, and the #[cfg(test)] modules in src files (grep for "mod tests").
Required coverage: storage migration, status endpoint, profile/workspace creation, record create/search, recall filters by profile/workspace/repo, writeback rejects secret, prompt-injection rejected, conclusion creates record, checkpoint stores and recalls, local import preview writes nothing, local import apply writes records, repeated apply idempotent, work-to-personal export denied, forget archives by default, export omits secret_blocked, HTTP smoke, CLI smoke.
Find required tests that are MISSING or that are present but don't actually assert the behavior (vacuous tests). Report gaps only.`,
  },
]

phase('Review')
const results = await pipeline(
  DIMENSIONS,
  (d) =>
    agent(d.prompt, {
      label: `review:${d.key}`,
      phase: 'Review',
      schema: FINDINGS_SCHEMA,
      agentType: 'Explore',
    }),
  (review, d) => {
    const findings = (review?.findings || []).filter((f) => f.severity !== 'low' || f.file !== 'none')
    if (findings.length === 0) return { dimension: d.key, confirmed: [] }
    return parallel(
      findings.map((f) => () =>
        agent(
          `Adversarially verify this codex-memoryd audit finding. Default to is_real=false unless the code clearly confirms it.
Finding: ${f.title}
Severity claimed: ${f.severity}
Location: ${f.file}
Detail: ${f.detail}
SPEC ref: ${f.spec_ref}

Read the cited file(s) under ${ROOT} and confirm or refute with specific code evidence (quote the relevant lines). A finding is only real if it is a genuine SPEC violation or correctness bug present in the current code.`,
          { label: `verify:${d.key}:${f.title.slice(0, 24)}`, phase: 'Verify', schema: VERDICT_SCHEMA, agentType: 'Explore' },
        ).then((v) => ({ ...f, verdict: v })),
      ),
    ).then((verified) => ({
      dimension: d.key,
      confirmed: verified.filter(Boolean).filter((f) => f.verdict?.is_real),
    }))
  },
)

const confirmed = results.flat().filter(Boolean).flatMap((r) => r.confirmed)
log(`Confirmed ${confirmed.length} finding(s) across ${DIMENSIONS.length} dimensions`)

return {
  confirmed,
  by_dimension: results.flat().filter(Boolean).map((r) => ({ dimension: r.dimension, count: r.confirmed.length })),
}
