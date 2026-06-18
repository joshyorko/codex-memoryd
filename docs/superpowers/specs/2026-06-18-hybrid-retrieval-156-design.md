# Hybrid Retrieval #156 Design

## Context

Issue `#156` requests the first tranche of hybrid retrieval. In this tranche, there is **no live `/v1/recall` behavior change**. This includes no changes to admission logic and no changes to ranking behavior in production.

## Decision

For this tranche, we will use a local prototype that is disabled by default. The prototype is for evaluation only.
We will run reciprocal-rank fusion against a deterministic fixture corpus to measure potential long-history recall gains and keep all conclusions within the local test context.

`recall_not_authority` remains unchanged.

## Options

1. **SQLite FTS baseline**
   - Lowest operational risk and the fewest moving parts.
   - Strong, predictable keyword matching with low complexity.
   - Weakness: poorer semantic matching on paraphrased or long-history context.

2. **SQLite vector extension/table**
   - Keeps vectors in the local SQLite store.
   - Provides stronger semantic retrieval on fixture evaluations.
   - Increases local disk usage and introduces extension migration/dependency risk.

3. **Sidecar local vector artifact**
   - Keeps vector data in a separate local artifact.
   - Avoids coupling vector runtime changes to the main recall path.
   - Adds lifecycle and index-maintenance overhead for the sidecar.

4. **DuckDB / Parquet-like local artifact**
   - Useful for offline experiments, reproducible evaluation, and batched benchmark runs.
   - Good for analysis workflows and large local datasets.
   - Adds an extra abstraction layer versus direct recall-path integration.

5. **Hosted vector DB**
   - Explicitly rejected in this tranche.
   - Adds external network calls, auth, and operational coupling.
   - Contradicts the local-first, prototype-only constraints.

## Trade-offs

- **Latency**: SQLite FTS remains the fastest baseline. Hybrid retrieval adds embedding generation + fusion overhead.
- **Disk/storage cost**: vector approaches increase local footprint roughly in proportion to item count and embedding dimension.
- **Recall**: hybrid retrieval is expected to improve long-history recall and will be validated against deterministic fixtures.
- **Precision**: improved semantic matching can increase retrieval quality, but may also increase noise depending on merge policy.
- **Token budget**: better recall may increase retrieved context volume, which can increase context-token consumption unless bounded.
- **Operations risk**: vector migration, extension lifecycle, and index rebuilds introduce additional local operational risk.

## Hard constraints

- Local prototype is **disabled by default**.
- No hosted vector DB.
- No live `/v1/recall` behavior change, ranking change, or admissions-path change in this tranche.
- `recall_not_authority` remains in effect.

## Recommendation

Ship only the disabled-by-default local prototype and evaluate it with deterministic fixtures. If the measured latency, recall, precision, token-budget impact, and operational risk are favorable, plan a later tranche for production gating and rollout.
