# Red-team regression corpus

Adversarial fixtures for the memory-poisoning regression suite
(`tests/redteam_suite.rs`, issue #148). A memory system is a control surface:
these cases prove that unsafe content is never admitted into normal recall,
search, cards, adapters, export, or procedures.

The competitive landscape (`docs/competitive-landscape.md`) shows that no major
hosted memory system defends the write path — Mem0 self-acknowledges it has "no
built-in mechanism to validate whether a memory entry is safe before it's
persisted", and the OWASP "Agent Memory Guard" issues sit open and
unimplemented in LangMem and Letta. This suite is the regression proof that
codex-memoryd does defend it, deterministically and offline.

## Attack classes

| Class | What it tries | Expected defense |
| --- | --- | --- |
| `poisoned_episode` | A successful-looking episode carrying an injection payload. | Procedure derived from it is quarantined, never active; payload never recalled. |
| `delayed_trigger` | Benign-looking memory with a dormant "when recalled, do X" instruction. | Blocked at write (injection) or quarantined; withheld from recall. |
| `cross_profile_bleed` | Work-confidential content sought from the personal profile. | Boundary deny; never appears in personal recall/export. |
| `unsafe_adapter_export` | Quarantined/secret content that must not reach an adapter view. | Omitted from adapter export and context packs. |
| `procedure_poisoning` | An episode whose summary is a prompt-injection masquerading as a step. | Candidate quarantined with an `unsafe_content` reason; never applied active. |
| `stale_over_avoidance` | A retired/superseded "scar" that should not dominate recall forever. | Withheld from default recall once retired; still inspectable on request. |

## Rules

- **No real secrets or live payloads.** All values are synthetic.
- **Triage, not silence.** When a gate fires, the suite records *which* gate
  (write-policy, quarantine, boundary, activation) so a failure is actionable.
- **Withheld diagnostics must not leak the unsafe content** — only counts and
  reason codes.
