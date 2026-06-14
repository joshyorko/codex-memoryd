# Competitive Landscape: AI Memory Systems

> Positioning for **codex-memoryd** — a local-first, provenance-tracked, policy-gated AI memory substrate.
> **"Memory is recall, not authority."**
>
> Scope: survey of the leading agent-memory systems and the benchmarks they cite, focused on
> finding **testable, local, deterministic** dimensions where a reviewable substrate wins.
> All competitor accuracy/cost numbers below are **self-reported and graded by LLM-as-judge**
> unless noted — that fact is itself the central exploitable weakness.

Last updated: 2026-06-14.

---

## TL;DR — the structural gap

Every hosted competitor shares the same DNA: **LLM-driven, non-deterministic memory
formation, graded by a lenient LLM judge, with no provenance ledger, no policy gate on
writes, no abstention contract, and no cross-tenant isolation guarantee.** Their benchmarks
(LOCOMO, DMR) are small, saturated, and have documented label noise (~6.4% wrong answer
keys) and a judge that accepts ~63% of intentionally-wrong answers. None of them measure
poisoning resistance, profile bleed, staleness correctness, or provenance coverage —
which are exactly the things a local, reviewable substrate can prove deterministically in CI.

---

## 1. Mem0 (mem0ai) + LOCOMO

**Core idea.** A two-phase memory pipeline over conversations: an *extraction* phase
(LLM pulls salient facts from a rolling summary + recent-message window) and an *update*
phase where an LLM tool-call chooses **ADD / UPDATE / DELETE / NOOP** against the top-k
similar existing memories. Retrieval is dense vector top-k. `Mem0g` is a graph variant
(Neo4j) that stores entity/relationship triplets and marks conflicting edges *invalid*
rather than deleting them. All LLM ops use GPT-4o-mini.
([paper arXiv:2504.19413](https://arxiv.org/abs/2504.19413),
[HTML](https://ar5iv.labs.arxiv.org/html/2504.19413))

**Eval methodology + metrics.** Benchmarked on **LOCOMO**. Primary metric is **J score
(LLM-as-judge, GPT-4o-mini)**; also F1, BLEU-1, search/total latency (p50/p95), and token
cost per conversation. Baselines: OpenAI memory, LangMem, Zep, A-Mem, RAG, full-context.
([HTML 2504.19413](https://ar5iv.labs.arxiv.org/html/2504.19413))

**Headline claims.** "**26%** relative improvement in J over OpenAI", "**91%** lower p95
latency", "**>90%** lower token cost." ([abstract](https://arxiv.org/abs/2504.19413))
Newer marketing pages report different/higher numbers (92.5 LoCoMo) than the paper
(66.88 J). ([mem0.ai/research](https://mem0.ai/research))
**Caveat:** baseline for the latency/token claims is ambiguous — the abstract implies
OpenAI, the [blog](https://mem0.ai/blog/memory-hierarchy-in-ai-systems-from-sensory-to-semantic)
frames them vs full-context. Note the paper's own table shows **full-context (72.90 J)
beat Mem0 (66.88 J)** — Mem0's pitch is the latency/token trade, not top accuracy.

**Exploitable weaknesses.**
- Entire pipeline + judge run on a hosted, non-deterministic LLM; the eval grade is not reproducible.
- An [AgentOS audit](https://agentos.sh/blog/memory-benchmark-transparency-audit/) found
  the LOCOMO judge accepted **62.81%** of intentionally-wrong-but-topical answers, and a
  **6.4% ground-truth error rate** (99/1540 answers) — gaps below ~6 J points are noise.
- [Zep's rebuttal](https://blog.getzep.com/lies-damn-lies-statistics-is-mem0-really-sota-in-agent-memory/)
  alleges Mem0 mis-implemented competitors (timestamps appended to text, sequential search).
- No provenance/audit, no policy gating on writes, no poisoning/injection defense, no
  tenant-isolation guarantee — entirely out of scope for both Mem0 and LOCOMO.

---

## 2. Zep / Graphiti + the DMR benchmark

**Core idea.** **Graphiti** is a self-hostable temporal knowledge-graph engine
(Zep is the hosted SaaS on top). Three subgraphs — raw *episodes*, LLM-extracted
*semantic entities/edges*, and *community* clusters. Its signature is a **bi-temporal
model**: every edge carries `valid_at`/`invalid_at` (when true in the world) and
`created_at`/`expired_at` (when the system learned it). Conflicting facts are
**invalidated, not deleted**, preserving history; episodes give per-fact lineage.
Hybrid retrieval = cosine + BM25 + graph BFS, fused via RRF/MMR.
([paper arXiv:2501.13956](https://arxiv.org/abs/2501.13956),
[HTML](https://arxiv.org/html/2501.13956v1),
[GitHub](https://github.com/getzep/graphiti))

**Eval methodology + metrics.** Two benchmarks: **DMR** (Deep Memory Retrieval, inherited
from MemGPT — a 500-conversation MSC subset, ~60 messages each, one QA pair; GPT-4 judge +
ROUGE-L) and **LongMemEval** (~115k-token histories; accuracy + latency + avg context
tokens). ([HTML 2501.13956](https://arxiv.org/html/2501.13956v1))

**Headline claims.** DMR: **94.8%** (Zep, gpt-4-turbo) vs **93.4%** (MemGPT); 98.2% with
gpt-4o-mini. LongMemEval: **+18.5%** accuracy over full-context (gpt-4o, 71.2% vs 60.2%),
**~90%** latency reduction (28.9s→2.58s), context tokens cut 115k→1.6k. But
**single-session-assistant regressed −17.7%** — the graph abstraction loses info vs full
context. ([HTML 2501.13956](https://arxiv.org/html/2501.13956v1))

**Exploitable weaknesses.**
- Full Zep is metered cloud ([pricing](https://getzep.com/pricing)); data egress unless BYOC.
- Graph construction *requires* an LLM with structured output and is non-deterministic;
  Zep added "entropy-gated" dedup in v1.0 to cut ~75% of LLM calls — an implicit admission
  of the cost/variance problem. ([blog](https://blog.getzep.com/graphiti-hits-20k-stars-mcp-server-1-0))
- DMR is small and **saturated** (~94–98%) by Zep's own admission — weak discriminative power.
- No documented prompt-injection defense or policy gate on memory writes.

---

## 3. Honcho (Plastic Labs) — user modeling / theory of mind

**Core idea.** Reasoning-first user modeling, not chunk storage. Models *Peers / Sessions /
Workspaces*; builds **representations** (deductive/inductive/abductive *conclusions* +
summaries + peer cards). Explicit **theory-of-mind / perspective-taking**: `observe_others`
builds a peer's view of another peer from only what they've seen, so "Alice's view of Bob"
differs from "Charlie's view of Bob." Queried via the Chat endpoint (`peer.chat()`, formerly
the "Dialectic API"). Backed by a fine-tuned Qwen3-8B ("Neuromancer").
([core concepts](https://honcho.dev/docs/v3/documentation/core-concepts/representation.md),
[GitHub](https://github.com/plastic-labs/honcho),
[Neuromancer](https://plasticlabs.ai/neuromancer))

**Eval methodology + metrics.** LongMemEval, LoCoMo, and BEAM ("beyond a million" tokens),
all LLM-as-judge. ([benchmarks post](https://plasticlabs.ai/blog/research/Benchmarking-Honcho),
[evals.honcho.dev](https://evals.honcho.dev)) **No dedicated theory-of-mind /
trait-accuracy benchmark** — they proxy user-model quality with long-conversation QA.

**Headline claims.** "SOTA on LongMem, LoCoMo, BEAM; Pareto-dominant on accuracy, cost,
speed, tokens" ([honcho.dev](https://honcho.dev)). LongMem S = 90.4%; LoCoMo = 89.9%;
60–90% token savings. All self-reported, LLM-judged.

**Exploitable weaknesses.**
- Cloud-default (`api.honcho.dev`); can self-host (AGPL) but that's not the marketed posture.
- Persistent, cross-session **inferred** user profiles (infers "budget-conscious" with no
  one saying it) — a privacy surface; docs have no privacy/security page; deletion is coarse/async.
- No documented provenance/audit/derivation-explainability; **no testable abstention** — Chat
  always "synthesizes a coherent natural language response," no machine-checkable "I don't know."
- Async "deriver" → read-after-write staleness. Even temp-0 "does not equal determinism" (their words).

---

## 4. LangMem (LangChain) — procedural/semantic/episodic split

**Core idea.** An **SDK** (not a substrate) over a LangGraph `BaseStore`. Three memory types:
**semantic** (facts, as a collection or schema'd profile), **episodic** (past experiences —
"no opinionated utilities yet"), and **procedural** ("how an agent should behave," stored as
**updated instructions in the prompt**). Memory tools (`create_manage_memory_tool` /
`create_search_memory_tool`) plus hot-path vs background formation; procedural memory =
prompt-optimizer rewrites (`metaprompt`/`gradient`/`prompt_memory`). All ops are LLM-decided.
([conceptual guide](https://langchain-ai.github.io/langmem/concepts/conceptual_guide/),
[launch](https://www.langchain.com/blog/langmem-sdk-launch),
[GitHub](https://github.com/langchain-ai/langmem))

**Eval methodology.** **None published** — no metrics, no benchmark. Positioned as a toolkit.

**Headline claims.** Agents "learn and improve over time"; storage-agnostic; background manager
"automatically extracts, consolidates, updates." ([launch](https://www.langchain.com/blog/langmem-sdk-launch))

**Exploitable weaknesses.**
- Ships no durable store; `InMemoryStore` loses data on restart. Non-deterministic formation.
- Documented memory schema is `content / id / action` only — **no source/provenance field**.
- **No policy gate** (only a free-text `instructions` + CRUD-verb whitelist); no quarantine /
  injection defense — memories are LLM-extracted from conversation and written with no filter.
- **Procedural memory = always-on prompt text** ("data-independent"): no activation trigger,
  no precondition, no guardrail, no verification, no termination lifecycle.

---

## 5. Letta / MemGPT — memory hierarchy, self-editing memory

**Core idea.** "LLM as OS": treat the context window as RAM and external storage as disk.
**Main context** = system instructions + a read/write *working context* + a FIFO message queue;
**external context** = archival + recall storage, reached only via function calls. Memory is
**self-edited** — the agent autonomously moves/rewrites items (e.g. "Boyfriend" → "Ex-boyfriend").
A queue manager flushes ~50% of messages with a **lossy recursive summary** at overflow. Letta
productizes this as **memory blocks** (always-in-context, `read_only` flag) over a four-tier
hierarchy, with a newer git-tracked **MemFS**.
([MemGPT arXiv:2310.08560](https://arxiv.org/abs/2310.08560),
[Letta docs](https://docs.letta.com/guides/core-concepts/memory/memory-blocks))

**Eval methodology + metrics.** Introduced **DMR** (MSC + synthesized session 6; GPT-4 judge +
ROUGE-L). Also Document QA (NaturalQuestions, **50 questions**), nested KV retrieval (140 pairs),
and a conversation-opener task. ([2310.08560](https://arxiv.org/abs/2310.08560))

**Headline claims.** DMR: GPT-4 32.1% → **+MemGPT 92.5%**; GPT-4-turbo 35.3% → **93.4%**.
Nested-KV: GPT-4 baseline hits 0% by 3 nesting levels; MemGPT "unaffected by nesting depth."

**Exploitable weaknesses.**
- "Memory edits and retrieval are entirely self-directed" — the LLM may silently drop, summarize
  away, or hallucinate facts; summarization is explicitly lossy.
- **No immutable audit trail**: self-editing overwrites in place (destroys the prior value).
  Letta's git-tracked MemFS is an implicit acknowledgment of this gap.
- No policy gate on what may be persisted → adversarial input can inject false persistent memories.
- DMR/Doc-QA/KV are tiny and **near-saturated** (92–93%) → little headroom, weak discrimination.

---

## 6. Procedural-memory quality literature — what defines a *quality* procedure

**Cognitive roots.** SOAR/ACT-R procedural memory = **IF-THEN condition-action production
rules**, executed by how well **preconditions** match working memory, with utility-based
conflict resolution among competing rules.
([ACT-R](https://en.wikipedia.org/wiki/ACT-R),
[ACT-R+declarative paper arXiv:2505.05083](https://arxiv.org/abs/2505.05083))

**CoALA** formalizes agent memory as procedural / semantic / episodic and warns procedural
memory must be designer-initialized and that writing to it is "significantly riskier" — it
"can easily introduce bugs," risking functionality and alignment.
([CoALA arXiv:2309.02427](https://arxiv.org/abs/2309.02427))

**Modern definition (ProcMEM / Skill-MDP).** A quality skill is a tuple of exactly the
lifecycle codex-memoryd models:
1. **Activation condition** — NL description of observable context where the skill applies (trigger/precondition).
2. **Execution procedure** — ordered action sequence, reused without re-deriving reasoning.
3. **Termination condition** — explicit stop predicate `β(s)=1`.
Quality is enforced by a **gate** (admits only positive-scoring valid candidates =
verification/guardrail) and **score-based pruning** of redundant/failing skills.
([ProcMEM arXiv:2602.01869](https://arxiv.org/abs/2602.01869))

**Quality metrics.** The **Proced-Mem** benchmark evaluates *procedural-memory retrieval in
isolation* (arguing current evals conflate retrieval with planning/execution) using
**MAP (primary), Precision@k, Recall@k, NDCG@k**. It documents a "generalization cliff"
(30–42% MAP drop on novel vocabularies) and a granularity reversal across pattern levels.
A retrieved-but-wrong procedure = a **false activation**.
([Proced-Mem (MemAgents @ ICLR 2026)](https://openreview.net/pdf?id=4YhU3BZgoZ),
[code](https://github.com/qpiai/Proced_mem_bench))

**Takeaway for codex-memoryd:** its procedure schema (`activation_query`, `steps`,
`guardrails`, `termination_condition`, `source_episode_ids`, `confidence`, `state` ∈
candidate/active/retired/failed in `migrations/0007_procedures.sql`) is a direct,
*reviewable* implementation of the literature's quality criteria — most competitors have
none of this (LangMem's "procedural memory" is just prompt text).

---

## 7. LOCOMO & LongMemEval — exactly what they test

### LOCOMO ([arXiv:2402.17753](https://arxiv.org/abs/2402.17753), [HTML](https://arxiv.org/html/2402.17753v1))
- **Construction:** machine-generated + human-edited multi-session dialogues from personas +
  temporal event graphs. Paper: 50 conversations, avg 304.9 turns / 19.3 sessions / 9,209 tokens.
  **The publicly scored QA subset is only ~10 dialogues / 1,986 QA pairs (1,540 non-adversarial).**
- **Question types:** single-hop (36%), multi-hop (14.6%), temporal (20.6%), open-domain (3.9%),
  **adversarial/unanswerable (24.9%)** — the adversarial split is a proto-abstention test.
- **Metrics:** F1 + recall@k (paper); FactScore + ROUGE for summarization. The **J score
  (GPT-4o-mini judge)** used downstream is a later convention, not the paper's primary metric.
- **Criticisms:** small/saturated (fits in context — full-context ~73 J beat Mem0 ~68 J);
  **6.4% label error**; **62.81%** judge leniency; no standardized pipeline (reproduction
  failures of 92% → 38%). ([Penfield audit](https://dev.to/penfieldlabs/we-audited-locomo-64-of-the-answer-key-is-wrong-and-the-judge-accepts-up-to-63-of-intentionally-33lg),
  [bloo-mind](https://essays.bloo-mind.ai/posts/2026-05-20-mem-eval/),
  [AgentOS](https://agentos.sh/blog/memory-benchmark-transparency-audit/))

### LongMemEval ([arXiv:2410.10813](https://arxiv.org/abs/2410.10813), [HTML](https://arxiv.org/html/2410.10813v2), [repo](https://github.com/xiaowu0162/LongMemEval), ICLR 2025)
- **500 curated questions** in timestamped, scalable chat histories; commercial assistants drop
  **~30%** accuracy over sustained interaction. Variants: **_S** (~115k tok), **_M** (~1.5M tok),
  **_Oracle** (evidence only).
- **Five core abilities (exact):** (a) **Information Extraction**, (b) **Multi-Session Reasoning**,
  (c) **Temporal Reasoning**, (d) **Knowledge Updates** — recognize a changed user fact and use the
  *latest* value (the superseded/stale case), (e) **Abstention** — detect questions whose answer
  is absent and reply "I don't know" (30 `_abs` false-premise items).
- **Metrics:** QA accuracy via a gpt-4o judge (>97% human agreement); **Recall@k / NDCG@k** at
  both turn level (`has_answer`) and session level (`answer_session_ids`); needle-in-haystack via
  distractor sessions (ShareGPT + UltraChat).
- **Local deterministic reimplementation:** seed a fixed haystack with known evidence + filler;
  embed + cosine top-k scored by Recall@k/NDCG@k offline; model knowledge-updates as a
  versioned (attribute, timestamp) store where the answer is the max-timestamp value; abstention
  items have no matching key → correct = "I don't know"; **grade by normalized string/regex/numeric
  match, not an LLM judge** — which directly fixes the LOCOMO judge-leniency/non-determinism flaw.

---

## 8. How codex-memoryd can be demonstrably better

codex-memoryd's deterministic, model-free eval harness (`src/eval.rs`, commit `9fb5fec`;
in-memory SQLite, boolean scoring, fixed fixtures, `--format json`) already emits most of the
dimensions below. Each maps to a competitor weakness no hosted system measures. **The framing:
we don't claim higher LLM-judge QA accuracy — we claim properties they cannot test at all.**

| # | Dimension | Proposed metric definition | Already in `eval.rs`? | Targets which weakness |
|---|-----------|----------------------------|-----------------------|------------------------|
| 1 | **Poison acceptance rate** | Of N adversarial intake records (secrets/injection in body *and* nested metadata), fraction admitted to recall. Target **0.0**. | Yes — `poison_acceptance_rate`, `admission_precision/recall`; `poison_intake` fixture; 25+ secret patterns + injection patterns in `policy.rs` | Mem0/Zep/LangMem/MemGPT: no injection defense or policy gate on writes |
| 2 | **Cross-profile bleed rate** | Of all cross-profile recall/export attempts violating the boundary matrix, fraction allowed. Target **0.0**. | Yes — `cross_profile_bleed_rate`; `cross_profile_bleed` fixture; 16-rule export matrix in `policy.rs` | No competitor guarantees tenant/profile isolation; Honcho persists cross-session profiles |
| 3 | **Stale/superseded error rate** | Of queries whose fact was superseded, fraction answered with the *old* value (should serve latest or withhold). Mirrors LongMemEval Knowledge-Updates, scored deterministically. | Partial — `supersession_accuracy`; trust state `superseded`, 120-day stale gate in `recall.rs`. **Add:** explicit stale-error counter | LOCOMO "doesn't test knowledge updates"; self-editing systems overwrite lossily |
| 4 | **Abstention correctness** | On items with no supporting evidence, abstention precision/recall for emitting "recall withheld / unknown" vs fabricating. Mirrors LongMemEval Abstention + LOCOMO adversarial, deterministic. | **Gap** — add an abstention fixture family + metric | Honcho/LangMem have no machine-checkable "I don't know"; LOCOMO judge accepts 63% of wrong answers |
| 5 | **Provenance coverage** | Fraction of admitted memory records with ≥1 evidence-ledger row (source_kind/id/hash/safe_summary). Target **1.0**. | Partial — `evidence_coverage` (citations > 0); append-only ledger `migrations/0004`. **Add:** ledger-row coverage %, not just citation presence | Mem0/LangMem schema has no source field; MemGPT overwrites in place; none expose lineage |
| 6 | **Procedure activation precision / recall** | For procedures with `activation_query`, precision/recall of activating the *right* procedure for a query context (Proced-Mem style Precision@k / Recall@k). | Partial — `procedure_recall_success`; full schema in `migrations/0007`. **Add:** P@k/R@k over a labeled context set | LangMem procedural = always-on prompt (no triggers); CoALA warns procedural writes are risky |
| 7 | **False-activation rate** | Fraction of queries where a procedure activates whose preconditions/guardrails do *not* hold (Proced-Mem "generalization cliff" analog). Target low. | **Gap** — `delayed_trigger_rate` is hardcoded 0.0. Implement against negative fixtures | No competitor models termination/guardrails, so none can even measure this |
| 8 | **Recall-not-authority conformance** | Fraction of recalled facts + adapter exports carrying `authority: "recall_not_authority"` and admission metadata. Target **1.0**. | Yes — enforced in `eval.rs` + `conformance.rs` across 8 adapters | No competitor distinguishes recall from authority; self-editing memory *is* authority |
| 9 | **Eval determinism / reproducibility** | Byte-identical `eval substrate --format json` across runs/machines (no network, no LLM, fixed seeds/timestamps). | Yes — model-free, in-memory, boolean-scored | LOCOMO/DMR grade with a non-deterministic, lenient LLM judge (~6.4% label error, 63% acceptance) |
| 10 | **Local-first / zero-egress** | No memory write/read/grade requires a hosted model or network call; data never leaves device. | Yes — SQLite, CLI/daemon/MCP, no external services in eval path | Mem0/Zep/Honcho are cloud-default; even self-host needs hosted LLMs for extraction |

**Net positioning.** Against LLM-judge QA accuracy alone, codex-memoryd should *not* pick a
fight — that turf is saturated and the judges are noisy. Instead, own the dimensions that are
**(a) safety/governance-critical, (b) deterministically testable locally, and (c) literally
absent from every competitor's published methodology**: poison resistance, profile isolation,
provenance coverage, abstention correctness, supersession handling, and a real procedure
lifecycle with measurable activation precision and false-activation rate. The strongest single
claim: *"Every number we publish is reproducible offline with no LLM in the loop"* —
a claim no system in §1–§5 can make.

**Near-term harness work to fully back the table:** add (4) abstention fixtures + metric,
(7) a real false-activation metric (retire the hardcoded `delayed_trigger_rate = 0.0`),
upgrade (3)/(5)/(6) from boolean to rate/coverage/P@k forms.

---

## Source caveats
- All competitor accuracy/cost figures are **self-reported and LLM-judged**; treat as marketing.
- LOCOMO audit figures (6.4% label error, 62.81% judge acceptance) are third-party
  (Penfield/AgentOS/bloo-mind), cross-corroborated across two independent write-ups.
- Mem0-vs-Zep score dispute is unresolved (Mem0 reported Zep 65.99 J; Zep claims 75.14 J).
- ProcMEM (arXiv:2602.01869) and Proced-Mem are 2026 venues; "false activation rate" is our
  framing of their Precision@k/Recall@k and gate-rejection mechanisms, not a verbatim term.
