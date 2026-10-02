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

## Isolated Linux lifecycle validation

Native lifecycle checks start detached fixture daemons. Run them with a reaping
PID 1 or the repository's child-subreaper launcher:

```sh
python3 scripts/run-with-child-reaper.py cargo test --test cli_smoke
python3 scripts/run-with-child-reaper.py scripts/v0.1-release-gate.sh
```

Use the launcher only for isolated synthetic checks. When the command exits or
is interrupted, it terminates and reaps the command's remaining descendants;
do not wrap a real operator service or `--include-dogfood` with it.

A non-reaping PID 1 can leave an exited daemon as a zombie. Linux `kill(pid, 0)`
still reports that PID as present, so `src/native_runtime.rs::process_alive`
cannot establish clean shutdown in that environment. Inspect `/proc/<pid>/status`
and PID 1 before classifying such a fixture failure as a startup regression.
Do not waive the shutdown assertion or change production lifecycle behavior to
make the test environment pass.

The agent that starts a release gate must keep ownership until it has the
command's exit receipt. Shared log files do not transfer ownership of an
agent-local execution session; do not hand off a still-running gate as verified.

Validate the launcher with
`python3 -m unittest discover -s tests -p test_child_reaper.py -v`.
Its `run` function owns the main command's exit status separately from adopted
child reaping and bounds signal cleanup. The canonical release gate includes
these launcher checks.
