# Dreamer loop — research (Phase 0)

This is a **research-first** document. It studies the problem of background
memory synthesis and proposes a `codex-memoryd`-native design. It deliberately
ships **no daemon and no schema migration**. The concrete CLI/API/storage/eval
proposal lives in the companion [`dreamer-loop-design.md`](./dreamer-loop-design.md).

> **Memory is recall, not authority.** Everything proposed here is subordinate to
> the same rule the rest of `codex-memoryd` follows ([`README.md`](../README.md),
> [`SPEC.md`](../SPEC.md) §10.4): synthesized memory informs an agent but never
> overrides current user instructions, repository files, `AGENTS.md`, explicit
> policy, or verified current state.

## 1. What a "Dreamer loop" is here

A **Dreamer loop** is a background / offline memory-synthesis pass that turns
**safe evidence** already captured by `codex-memoryd` — visible turns,
conclusions, checkpoints, imported local memories, and existing active records —
into **durable, inspectable, provenance-backed** memory records. It compresses
repeated evidence into stable facts, demotes or rewrites stale facts, and
supersedes outdated facts with newer ones, instead of letting memory grow
forever as raw logs. It is evidence-backed consolidation: Dreamer proposes
candidates, but policy and deterministic thresholds decide whether they are
accepted, quarantined, or rejected.

The motivation is personal-agent leverage: reduce repeated context loading and
token burn across ChatGPT, Codex, devcontainers, homelab, OSS, work-adjacent,
and side-project workflows — **without** building a hosted platform or hiding
memory state behind an opaque product surface.

## 2. Public inspiration — and what we explicitly do *not* claim

The only external influence is **public writing about ChatGPT memory
"dreaming"**:

- OpenAI, "ChatGPT memory and dreaming":
  <https://openai.com/index/chatgpt-memory-dreaming/>

### Non-claims (must hold in all docs and PRs)

- **ChatGPT Dreaming is not open source.** We study public behavior and public
  writing only. We do **not** have, use, or reproduce OpenAI's private memory
  internals.
- We do **not** claim compatibility or interoperability with ChatGPT's memory
  system.
- We do **not** imply knowledge of OpenAI's internal architecture.
- We do **not** position this as private OpenAI internals, a general memory
  platform, or an autonomous background writer. The lane is a local-first
  coding-agent memory primitive with preview/apply, policy gates, provenance,
  supersession, and fail-open behavior.
- We avoid product-copy framing; this repo studies public behavior only.

### Takeaways worth researching (public-behavior level only)

- Memory should not depend only on explicit "remember this" commands.
- Repeated interaction can reveal stable preferences, workflows, projects, and
  constraints.
- Stale, time-sensitive facts need active rewriting or demotion.
- Memory should compress/synthesize evidence, not accumulate raw logs.
- Users need correction / deletion / export boundaries.
- Memory must remain contextual recall, not authority.

All of these map onto contracts `codex-memoryd` already exposes, so the Dreamer
loop is an additive synthesis pass over existing primitives rather than a new
product.

## 3. Existing `codex-memoryd` primitives the loop builds on

The repository already contains most of the building blocks; the Dreamer loop is
an orchestration layer over them, not a rewrite.

| Capability | Where today |
| --- | --- |
| HTTP surface (`/v1/status`, `/v1/recall`, `/v1/search`, `/v1/turns`, `/v1/conclusions`, `/v1/checkpoints`, `/v1/sync/local-codex-memory`, `/v1/forget`, `/v1/export`) | [`src/server.rs`](../src/server.rs) |
| Durable entities (`MemoryRecord`, `MemorySource`, `VisibleTurn`, `Conclusion`, `Checkpoint`) | [`src/domain.rs`](../src/domain.rs) |
| Recall ranking + `recall_not_authority` tagging | [`src/recall.rs`](../src/recall.rs) |
| Local-import preview/apply with source-hash + content-hash dedupe and stale-path supersession | [`src/ingest.rs`](../src/ingest.rs) |
| Policy gate: secrets, prompt-injection, hidden-reasoning markers, boundary export, oversized blobs, classification | [`src/policy.rs`](../src/policy.rs) |
| Storage, migrations, FTS5 probe + LIKE fallback, `archive_stale_path_records` | [`src/store.rs`](../src/store.rs) |
| Profile/workspace/repo boundaries framed as recall-not-authority | [`README.md`](../README.md), [`SPEC.md`](../SPEC.md) |

What is **missing** (the gap this research addresses):

- no explicit source-selection loop over turns, conclusions, checkpoints, and
  imported memories;
- no background/offline synthesis pass;
- no first-class stale/conflicting/superseding workflow outside local-import
  archival ([`src/store.rs`](../src/store.rs) `archive_stale_path_records`);
- no generated profile/workspace memory digest or summary view;
- no eval harness measuring whether synthesis improves future recall;
- no daemon scheduling story;
- no MCP/App access story beyond the HTTP provider API.

## 4. Research questions and findings

### 4.1 What should a Dreamer synthesize?

**Synthesize (high-signal, reusable):**

- stable user preferences (`preference`);
- recurring operating style / steering corrections (`workflow_pattern`);
- project and repo maps (`landmark`, `repo_convention`);
- commands that repeatedly work (`command`);
- gotchas / failure shields (`gotcha`);
- decisions and rationale (`decision`);
- task checkpoints (`task_checkpoint`);
- identity / profile details (`identity`);
- stale facts rewritten as historical or invalidated (supersession).

These map 1:1 onto `RecordType` in [`src/domain.rs`](../src/domain.rs), so no
new record taxonomy is needed.

**Do not synthesize:**

- one-off assistant proposals **not** adopted by the user;
- raw logs;
- secrets (already blocked by [`src/policy.rs`](../src/policy.rs));
- hidden reasoning (never stored; see [`SPEC.md`](../SPEC.md) §4.1.5);
- speculative facts not grounded in repeated evidence;
- work-confidential material crossing into a personal profile (boundary matrix,
  [`SPEC.md`](../SPEC.md) §10.3);
- generic advice that does not improve future agent performance.

**Finding:** synthesis is a *promotion* decision over existing evidence. The
key signal is **repetition / adoption**, not novelty. A candidate needs
corroborating evidence (e.g. repeated user steering across multiple turns)
before it is promoted.

### 4.2 What is the source model?

Evidence sources, all already persisted, are intentionally weighted
asymmetrically:

| Evidence class | Weight / role | Promotion rule |
| --- | --- | --- |
| User visible turns | Strong primary evidence | Repeated user steering or explicit user adoption can promote. |
| Explicit conclusions | Strong explicit evidence | Durable conclusions can promote with high confidence. |
| Checkpoints | Strong for task/repo state | Task state, next steps, repo gotchas, and conventions can promote when scoped. |
| Assistant visible turns | Weak evidence | Quarantine unless later adopted by user, conclusion, or checkpoint. |
| Imported local memories | Secondary/corroborating only | Support fresh primary evidence; never create active memory alone. |
| Existing active records | Conflict/supersession only | Detect contradiction, expiry, or replacement; never self-reinforce. |

A deterministic `subject_key` is the anchor for candidate grouping and
supersession. It is derived from the proposed record family, normalized subject
terms, and the profile/workspace/repo boundary so same-subject candidates compete
with each other instead of producing parallel active truths.

Future sources (only if sanitized and explicitly accepted): MCP / App events.

**Finding:** v1 does **not** need new source-selection tables. A **dream-run
watermark** (last processed `created_at` per profile/workspace) over the
existing tables is enough to bound each pass and keep it incremental. A durable
`dream_runs` table is justified for auditability; per-candidate rows are
deferred (see design doc §Storage).

### 4.3 What is the right preview/apply contract?

The loop must be **CLI-driven and explicit before it is ever daemonized**,
mirroring the existing `sync-local --preview/--apply` contract
([`src/ingest.rs`](../src/ingest.rs), [`SPEC.md`](../SPEC.md) §7).

- **Preview writes nothing durable** and reports: evidence scanned, candidate
  records, proposed archives/supersessions, rejected candidates with reasons,
  time-sensitive/stale candidates, estimated record/token impact, policy /
  boundary decisions, and confidence + provenance.
- **Apply is idempotent and policy-gated.** Re-running over the same evidence
  window yields `created: 0`, exactly like local-import re-apply.

The exact request/response shape is specified in the design doc.

### 4.4 What storage state is needed?

**Finding:** start minimal. v1 needs only:

- a durable `dream_runs` audit table (run id, profile, workspace, mode, status,
  window, model, input hash, summary, error);
- `memory_records.metadata.origin = "dreamer"` plus provenance fields on each
  synthesized record (no new per-candidate table).

Candidate state is report-local: `accepted`, `quarantined`, or `rejected`.
Accepted candidates may become memory records only through policy-gated apply.
Memory state is record metadata: `planned`, `active`, `blocked`, `completed`,
`historical`, or `superseded`. State is deliberately metadata-first so the
existing schema and recall path remain stable while evals harden behavior.

Per-candidate persistence (`dream_candidates`) is deferred until previews need
to be replayed/approved out-of-band. Preview output is computed and returned,
not stored, in v1.

### 4.5 How should staleness work?

Today recall has only an age-based hint (`STALE_DAYS = 120` in
[`src/recall.rs`](../src/recall.rs)). The Dreamer needs **content-aware** drift
detection. Drift-prone language to flag:

- relative time: `today`, `tomorrow`, `this week`, `next week`, `this weekend`,
  `currently`, `right now`, `soon`;
- planned vs. completed events ("will deploy" vs. "deployed");
- repo facts cheaply verifiable before reuse;
- version/date-sensitive third-party facts;
- active project status that decays quickly.

Required record metadata for Dreamer apply is additive (no migration —
`metadata` is already a free JSON `Value` on `MemoryRecord`):

```json
{
  "origin": "dreamer",
  "dream_run_id": "dream_...",
  "subject_key": "preference:repo-native-commands",
  "evidence_ids": ["turn_..."],
  "evidence_count": 3,
  "user_evidence_count": 3,
  "assistant_evidence_count": 0,
  "first_seen_at": "...",
  "last_seen_at": "...",
  "evidence_window": { "start": "...", "end": "..." },
  "state": "active",
  "drift_prone": true,
  "expires_at": "...",
  "valid_after": "...",
  "valid_until": "...",
  "supersedes": ["mem_..."],
  "historical_reason": null,
  "promotion_reason": "repeated user steering across 3 turns"
}
```

`supersedes` already exists as a first-class field on `MemoryRecord`; the
metadata mirror is for cases recorded only during a dream run.

### 4.6 What synthesis backend is acceptable?

Options considered:

1. heuristic-only MVP;
2. LLM-backed synthesis behind a trait;
3. hybrid: heuristic candidate selection + LLM synthesis + deterministic
   policy/storage gate.

**Finding:** adopt the **hybrid boundary**, but ship the **heuristic-only**
implementation first. The hard invariant is:

> **The model proposes; `codex-memoryd` validates and persists.** The LLM never
> receives direct write authority.

A `DreamSynthesizer` trait isolates the proposal step so a heuristic synthesizer
ships first and an LLM synthesizer can be swapped in later without touching the
policy/storage gate. Every proposed record still passes through
[`src/policy.rs`](../src/policy.rs) before any write.

### 4.7 How do we evaluate it?

A repeatable JSONL fixture format drives evals (specified in the design doc and
seeded under `tests/fixtures/dreaming/`). Eval questions are fixture-driven with `*.expected.json` sidecars:

- Did it promote the stable preference?
- Did it avoid storing the secret?
- Did it mark stale / time-sensitive facts correctly?
- Did newer evidence supersede older evidence?
- Did preview classify candidates into accepted / quarantined / rejected buckets?
- Did recall improve for a future prompt? This recall-before/after delta is the
  key proof metric, because extraction only matters if future coding-agent
  recall gets better without forbidden leakage.
- Did it preserve provenance?
- Is apply idempotent?

### 4.8 How does this relate to ChatGPT / MCP / Apps?

Researched separately and **deferred** to a later phase. Principles to preserve:

- `codex-memoryd` could expose MCP tools for `recall` / `search` / `conclude` /
  `checkpoint` / `dream_preview`;
- it could later be wrapped as an MCP-compatible service or ChatGPT app once
  auth and remote safety are explicit;
- **remote access must not weaken profile/workspace boundaries**;
- the first primitive must work **locally first**; DNS + auth come later.

## 5. Threat model

The Dreamer loop adds an automated write path, so it widens the attack surface.
Each risk below is mitigated by an **existing or required** gate.

| Threat | Mitigation |
| --- | --- |
| **Secret promotion** — a credential repeated across turns gets synthesized into a durable record | Every proposed record passes [`src/policy.rs`](../src/policy.rs) secret detection before write; rejected candidates are reported, never stored. |
| **Prompt injection** — evidence contains "ignore previous instructions" style payloads that become durable instructions | Injection detection in [`src/policy.rs`](../src/policy.rs) (§10.2) runs on every candidate; synthesized memory is tagged `recall_not_authority` so it cannot override instructions even if it slips through. |
| **Hidden-reasoning leakage** — synthesis pulls in model chain-of-thought | Hidden reasoning is never stored ([`SPEC.md`](../SPEC.md) §4.1.5); the loop only reads visible turns / conclusions / checkpoints / imported memories. |
| **Work → personal leakage** — work-confidential evidence synthesized into a personal-profile record | The loop runs **within a single profile/workspace** and reuses the boundary matrix ([`SPEC.md`](../SPEC.md) §10.3); cross-profile promotion is denied by default. |
| **Model overreach** — an LLM synthesizer fabricates or over-generalizes facts and writes them directly | Model output is a *proposal* only; the deterministic policy/storage gate validates and persists. The LLM has no write authority. Confidence + `promotion_reason` + `evidence_window` provenance are required on every record. |
| **Runaway growth / cost** — an unbounded loop floods storage or burns tokens | Preview-first, bounded evidence window per run (watermark), idempotent apply, and (Phase 3) bounded batch size + rate/cost controls. |
| **Silent corruption of good memory** — supersession archives a still-valid record | Supersession is recorded with provenance (`supersedes`, `promotion_reason`); archived records are recoverable (archive, not hard-delete) consistent with [`src/store.rs`](../src/store.rs) `archive_stale_path_records` and `/v1/forget` archival default. |

## 6. Non-goals (for the Dreamer loop)

- Not a daemon in the first PR(s); preview/apply must be trustworthy first.
- Not a hosted platform, dashboard, or cloud service.
- Not a replacement for explicit `/v1/conclusions` — the loop augments, it does
  not replace, explicit memory.
- Not an authority surface; output stays `recall_not_authority`.
- Not a generic LLM agent; the model only proposes synthesized candidates.
- No new record taxonomy; reuse `RecordType`.

## 7. Acceptance check for this research issue

- [x] Research doc exists, citing only public sources and repo code.
- [x] Docs state ChatGPT Dreaming is not open source and is only public
      inspiration.
- [x] Proposed architecture fits existing provider boundaries (recall,
      policy gate, profile/workspace isolation, archival supersession).
- [x] Preview/apply contract specified (here + design doc).
- [x] Staleness / supersession rules specified enough to implement tests.
- [x] Threat model covers secrets, prompt injection, hidden reasoning,
      work/personal leakage, and model overreach.
- [x] Eval fixtures specified before implementation (design doc + seeded
      fixture directory).
- [x] First-PR plan is small and reviewable (docs-only Phase 0).

## 8. Phased plan (summary)

| Order | Issue | Deliverable | Writes? |
| --- | --- | --- | --- |
| 1 | #10 | Deterministic preview-only source gathering and Dream report | none |
| 2 | #11 | Fixture eval harness with `*.expected.json` sidecars and recall-before/after checks | none |
| 3 | #19 | Promotion thresholds, asymmetric evidence weighting, and candidate states | none |
| 4 | #13 | Staleness, memory state transitions, and supersession rules | gated when applied |
| 5 | #12 | Idempotent, policy-gated `dream --apply` with required provenance | gated |
| 6 | #20 | Dream-run audit table, safe counts, and watermarks | audit only / gated |
| 7 | #14 | Live Codex tap-release provider/hybrid smoke and fail-open proof | best-effort writes |
| 8 | #15 | Loopback/auth/status/fail-open reliability contract | n/a |
| 9 | #17 | One-command local install and first-run demo path | user-directed |
| 10 | #16 | Local-first coding-memory bakeoff and public proof package | n/a |
| 11 | #18 | Local MCP recall/search/conclude/checkpoint/dream-preview tools | preview no-write; apply deferred |

The detailed design for Phases 1–2 is in
[`dreamer-loop-design.md`](./dreamer-loop-design.md).
