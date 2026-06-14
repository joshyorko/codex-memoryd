# Policy regression corpus

Durable allow/deny/redaction fixtures for the write-path safety gate
(`codex_memoryd::policy`). The corpus turns one-off policy bugs — false
positives that blocked safe content, and false negatives that admitted unsafe
content — into permanent regression cases.

The corpus is exercised by `tests/policy_corpus.rs`, which loads every case,
runs it through the real policy functions, and reports per-category counts.

## Categories

| File | Meaning | Assertion |
| --- | --- | --- |
| `allow.json` | Safe content that previously triggered (or could trigger) a false positive. | `screen_string_value` returns `Accept`; `detect_secret` returns `None`. |
| `deny.json` | Unsafe content that must be rejected before durable write. | `screen_string_value` returns `Reject`; `code` matches `expect_code` when set. |
| `redact.json` | Content that may be summarized into the evidence ledger only after redaction. | `redact_secret_like` reports `redacted = true` and the output contains none of `must_not_contain`. |

## Rules

- **No real secrets.** Every value in `deny.json` / `redact.json` is synthetic:
  a documented vendor example or an obviously fake placeholder shaped to match
  a detector. Never paste a live credential.
- **Fragment secret-shaped values.** Secret-shaped cases use `content_parts`
  (and `must_not_contain_parts` for redaction), arrays the runner joins with no
  separator at load time. This keeps a contiguous secret-shaped literal out of
  the committed file so push-protection scanners do not flag it, while the
  policy gate still receives the full reconstructed string. Use plain `content`
  only for non-token shapes (injection phrases, `.env` dumps, PEM headers).
- **Names and notes are safe to commit** and are printed in test output.
- **Outputs are safe to commit**: the redaction assertion proves no raw value
  text survives.

## Adding a fixture after a dogfood failure

1. Reproduce the failure as the smallest content string that shows it.
2. If safe content was wrongly blocked → add a case to `allow.json`.
3. If unsafe content was wrongly admitted → add a case to `deny.json`
   (set `expect_code` to the stable reason code) or, if it should be summarized
   after redaction, to `redact.json` with the raw token(s) in `must_not_contain`.
4. Scrub any real value to a synthetic equivalent of the same shape.
5. Run `cargo test --test policy_corpus`. A new case that fails proves the gap;
   fix the detector in `src/policy.rs` until the whole corpus is green.
