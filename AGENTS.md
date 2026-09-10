# codex-memoryd — Agent Operating Rules

## Recursive self-improvement

Complete the requested work and improve the guidance that future agents use.
At task start, after a failure or discovery, and before handoff, check the
relevant documentation and skills against the actual source and verified
behavior.

- Repair stale, contradictory, or missing guidance in the nearest authoritative
  document or skill. Keep `AGENTS.md` for repository-wide rules, user-facing
  behavior in the matching documentation, and reusable procedures in the
  closest existing skill. Create a skill only when durable guidance has no
  suitable home.
- Include relevant documentation and skill updates in the same change or PR
  as the implementation. Correct the existing guidance instead of appending
  a competing rule or creating session logs and running lists of lessons.
- Capture source-backed discoveries, failure causes, verified remedies, and
  non-obvious constraints. Include source locations or verification commands
  so a future agent can recheck claims. Do not invent learning to satisfy this
  rule; report when no durable update is warranted.
- Validate changed guidance with the repository's applicable checks. Regenerate
  skill indexes or other generated documentation through their existing
  generators when applicable; edit the source, not generated copies.
- Keep improvements within the authorized task. This loop does not authorize
  unrelated changes, edits to personal memory or other repositories, expanded
  permissions, or weakening tests, security boundaries, and review gates.

## Required Codex connector review before merge

The ChatGPT Codex connector is configured to review every PR push automatically.
Before merging, agents must wait for at least one completed connector review
of the latest PR head and inspect its findings. After a new push, wait for
that head's automatic review; an earlier head's review does not satisfy this
gate.

- Use the automatic review. Do not post a manual review request, invoke an
  additional agent review, rerun a review job, or push solely to trigger another
  review unless the operator explicitly approves that additional review.
  Necessary implementation and fix pushes may receive their configured
  automatic reviews; they do not authorize extra agent-initiated reviews.
- Verify completion from live GitHub evidence tied to the head commit. A
  queued review, acknowledgment, reaction, silence, or green CI alone is not
  evidence that the connector finished reviewing.
- Address actionable findings and verify fixes. If a finding is disputed,
  provide evidence and surface the unresolved decision to the operator rather
  than silently treating it as resolved.
- If the connector review is pending, unavailable, or failed, keep the merge
  blocked and report that status. Do not substitute a local or subagent review
  for the connector, bypass this gate, or enable auto-merge or enqueue a merge
  before it is satisfied.
- Before merge or handoff, report the reviewed head, a link to the completed
  connector review, unresolved findings, and required check status. Review
  completion does not itself authorize merging or replace other merge gates.
