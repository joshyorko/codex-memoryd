# Agent-Agnostic Memory Substrate

> **Design slice for issues #43, #44, #45.**
> This is a substrate document, not an implementation plan. It defines the
> shape Codex Memory should grow into without breaking the current local-first,
> recall-not-authority contract.

**Goal:** define a memory substrate that can serve Codex, Honcho, and future
agent runtimes without baking one agent's lifecycle into the storage model.

**Architecture:** memory is split into three layers: immutable evidence,
subject-centered episodes, and projected durable records. The substrate keeps
raw provenance separate from synthesized claims so recall stays contextual,
preview stays mandatory, and policy can reject unsafe material before it ever
becomes durable memory.

**Tech Stack:** Rust service/store today; docs-first design for the substrate;
SQLite-backed persistence remains the local-first baseline.

---

## Why this exists

Current `codex-memoryd` docs already define a local-first portable memory
provider, Dreamer consolidation, and Codex integration. What they do not yet
name is the substrate underneath those features:

- what is the stable identity of "what this memory is about";
- what is a single interaction unit versus a durable fact;
- what evidence must be preserved to justify a projection;
- how to keep the design agent-agnostic instead of hard-coding Codex turn
  semantics into the core.

This slice answers those questions.

## Operating constraints

The substrate must preserve the current guardrails:

- local-first by default;
- recall is advisory, not authority;
- preview happens before apply;
- profile boundaries stay explicit;
- secrets, credentials, raw confidential logs, and hidden reasoning do not
  become durable memory;
- policy decisions stay visible and auditable.

## Comparison snapshot

| System | Core shape | What it optimizes | Gap for this project |
| --- | --- | --- | --- |
| Honcho | Workspace / peer / session / message / representation model with reasoning over stored data. | Stateful agent context and explicit reasoning over stored messages. | Closest baseline, but its primitives still center Honcho's agent model rather than a Codex-neutral substrate. |
| Zep | Temporal knowledge graph with graph-backed synthesis and historical relations. | Cross-session temporal recall and structured relationship maintenance. | Strong on graph reasoning, weaker on keeping a simple local-first evidence ledger and explicit preview/apply flow. |
| Letta | Agent memory around persistent assistants and managed memory flows. | Stateful assistant behavior over time. | Useful mental model, but too agent-instance-centric for a substrate that should outlive one assistant type. |
| LangMem | Framework memory layer for long-horizon agent personalization. | Retrieval and consolidation for long-lived conversational state. | Useful for agent memory patterns, but it is still a framework layer rather than a durable substrate with explicit provenance states. |
| Mem0 | Extract / consolidate / retrieve pipeline for long-term memory. | Practical production memory writing with compact retrieval. | Good write-path inspiration, but it treats memory as a consolidated store, not as subject + episode + ledger layers. |
| Claude Code | Agentic coding tool with instructions, sessions, and stored memories. | Fast coding-agent UX inside Claude surfaces. | Great product surface, but memory remains part of a single vendor agent experience, not a portable substrate. |
| Copilot | Instructions, session data, memory, Spaces, MCP, and agent surfaces across GitHub. | Integrated agent UX with policy/admin controls. | Broad product surface, but the memory shape is still tied to Copilot's platform and governance model. |
| MCP | Transport standard for tools, data, and workflows. | Interoperable context/tool connectivity. | MCP is a transport, not a memory model; it can carry memory APIs but should not define the substrate itself. |

Takeaway: the right substrate is not "which agent owns memory." It is "what are the universal memory units, how are they justified, and how do we keep them safe and portable."

## vNext primitives

### 1. Subject

A `Subject` is the stable identity of the thing memory is about.

Examples:

- a user preference;
- a repository convention;
- a project decision;
- a task thread;
- a recurring gotcha;
- a workspace-scoped working agreement.

Rules:

- A subject is not a fact.
- A subject may have many episodes.
- A subject can be shared by multiple evidence sources, but it keeps one stable
  key.
- A subject must carry profile and workspace boundaries.
- A subject may carry repo identity when the subject is repo-scoped.

Recommended fields:

- `subject_key`
- `subject_type`
- `profile`
- `workspace`
- `repo_id` or `null`
- `scope`
- `title`
- `state`
- `confidence`
- `created_at`
- `updated_at`
- `metadata`

The subject key should be deterministic and boring. It should group related
evidence without depending on one agent's internal session shape.

### 2. Episode

An `Episode` is an immutable unit of observed activity attached to a subject.

Episodes capture "what happened" rather than "what we decided memory should
be."

Examples:

- a visible turn;
- a conclusion;
- a checkpoint;
- an imported local memory fragment;
- a manually entered note;
- a safe derived summary.

Rules:

- Episodes are append-only.
- Episodes do not overwrite each other.
- Episodes retain provenance and source references.
- Episodes can be rejected, quarantined, or accepted for projection.
- Hidden reasoning is not an episode source.

Recommended fields:

- `episode_id`
- `subject_key`
- `episode_type`
- `source_id`
- `source_kind`
- `profile`
- `workspace`
- `repo_id` or `null`
- `actor`
- `summary`
- `raw_ref` or `null`
- `policy_state`
- `created_at`
- `metadata`

The summary must be safe to store and short enough to inspect. If a source needs
redaction, the episode stores the redacted form plus the policy decision, not
the forbidden text.

### 3. Evidence ledger

The evidence ledger is the append-only audit trail that explains why a subject
exists, why its state changed, and why a projection was allowed.

This is the missing layer between "raw input" and "durable memory record."

Ledger entries should track:

- source metadata;
- source hash;
- subject links;
- episode links;
- policy outcomes;
- safe summaries;
- confidence and provenance;
- supersession or conflict notes.

Recommended fields:

- `ledger_id`
- `source_id`
- `source_kind`
- `source_path`
- `source_hash`
- `profile`
- `workspace`
- `repo_id` or `null`
- `subject_key`
- `episode_id` or `null`
- `summary`
- `evidence_class`
- `policy_state`
- `redaction_state`
- `confidence`
- `created_at`
- `metadata`

Ledger rules:

- one source can emit many ledger rows;
- a ledger row should point to the smallest safe summary that explains the
  evidence;
- safe summaries are stored, raw secrets are not;
- if the source is rejected, the rejection is still ledgered;
- if evidence is superseded, the old ledger remains as history;
- if evidence is ambiguous, it stays visible as evidence and does not pretend
  to be a fact.

## Projection model

Memory records are projections from subject + episode + ledger, not the primary
truth.

That gives three clean layers:

1. Source and evidence are auditable.
2. Subjects group meaning.
3. Memory records are the compact recall surface.

This is the right split for Codex because it keeps:

- recall cheap;
- provenance intact;
- policy decisions explicit;
- preview-before-apply mandatory;
- future agent adapters from becoming schema owners.

## Issue #43: substrate direction

The substrate should be agent-agnostic by design:

- Codex turn shapes are one input, not the model;
- Honcho-style sessions are one adapter, not the core;
- MCP is one transport, not the substrate;
- memory should stay portable across local, daemon, and remote surfaces.

The document for #43 should therefore stay framework-neutral in language and
use "subject / episode / ledger / projection" as the stable vocabulary.

## Issue #44: Subject and Episode boundaries

Subject boundaries:

- one subject per durable concern;
- subjects are mutable only in metadata and lifecycle, not identity;
- subjects can be archived or superseded, but the key stays stable;
- subjects are not free-form note buckets.

Episode boundaries:

- one episode per observed event, note, or derived safe summary;
- episodes do not encode durable truth by themselves;
- episodes never replace subjects;
- episodes can be replayed, previewed, and suppressed without changing the
  source history.

This keeps tests green-friendly because the visible behavior can remain the same
while the internal vocabulary becomes more precise.

## Issue #45: evidence ledger MVP shape

The MVP should not try to solve full graph memory. It should implement a narrow
ledger that can answer:

- what source produced this claim;
- what subject did it attach to;
- what safe summary was stored;
- what policy allowed or denied it;
- what future projection can cite it.

Minimal viable rules:

- store scoped evidence records;
- store source metadata and source hashes;
- store safe summaries instead of raw reasoning;
- keep policy state on every ledger row;
- keep the raw source out of durable memory when unsafe;
- keep the ledger append-only.

That is enough to support future recall, supersession, export, and audit without
locking the store into one agent lifecycle.

## Phased implementation order

1. Add the substrate vocabulary to docs and align current terms.
2. Define `Subject` and `Episode` boundaries in the domain model docs.
3. Add evidence ledger rows as the audit bridge between sources and projections.
4. Make recall read from projections while preserving provenance links.
5. Make Dreamer and import flows emit ledger rows before durable projections.
6. Tighten boundary policy, redaction, and safe-summary rules.
7. Only then consider broader graph or cross-agent extensions.

## Acceptance shape

This slice is good enough if a reader can answer:

- what is the subject;
- what is the episode;
- what evidence justified the projection;
- why the system is still recall-not-authority;
- why preview still happens before apply;
- how profile boundaries and secret blocking stay enforced.

## Cross-links

- [README](../README.md)
- [SPEC](../SPEC.md)
- [Dreamer design](./dreamer-loop-design.md)
- [Codex integration](./codex-integration.md)
