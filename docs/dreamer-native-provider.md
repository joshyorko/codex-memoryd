# Subscription-backed Dreamer previews

Dreamer supports the existing HTTP providers and an operator-configured Linux
`command` adapter. The companion `scripts/dreamer_native_provider.py` uses
Hermes' public `resolve_provider_client` interface, including Codex subscription
authentication and Responses compatibility. It does not start a conversational
agent, register tools, execute model-produced actions, copy credentials, or
fall back to another provider/model.

## Configure the existing runtime

Run MemoryD where the selected interpreter can import the installed Hermes
package and use its own authenticated profile. A minimal Rust-only container
does not contain that interpreter or authentication owner. Use the native
runtime, or an explicitly provisioned deployment; do not mount a Docker socket
or paste an expiring OAuth token into the configuration.

Set `HERMES_HOME` and `PYTHONPATH` in the service environment to the intended
profile and installed Hermes source. Keep credentials in their existing owner.
Configure absolute executable/script paths:

```toml
[dream]
scheduler_enabled = true
scheduler_interval_seconds = 3600
idle_window_seconds = 900
max_batch_size = 8
max_candidates = 5
max_runtime_seconds = 45
scheduled_provider_enabled = true
provider_enabled = true
provider_adapter = "command"
provider_model = "gpt-5.6-luna"
provider_name = "hermes-openai-codex"
provider_command = ["/absolute/hermes/python", "/absolute/dreamer_native_provider.py", "--provider", "openai-codex", "--model", "gpt-5.6-luna"]
provider_timeout_seconds = 35
provider_max_response_bytes = 262144
```

The shipped companion explicitly permits Luna and Codex Spark and requires a
named provider, not `auto`. Other configured command implementations and the
existing HTTP adapter retain their own operator-selected provider integration.
Only the Codex/Luna companion has been exercised against a real subscription.

Job inputs cannot replace the configured command, model, or endpoint. An
additional HTTP endpoint in the configuration is not called during command
mode. Do not put secrets in argv. Operator-selected executables are trusted
code, not a security sandbox; they must stay in the foreground and keep their
children in their process group. The native companion has no tool executor.

## Budgets and review

Scheduled command mode uses the existing typed preview job: one provider call,
zero provider retries, bounded input/output, and a deadline covering pipe I/O.
The Linux runner uses nonblocking pipes, not detached reader/writer threads.
An oversized response is stopped at the smaller job/configured byte limit.
Failure does not advance the successful scheduled watermark. Process-group
children are terminated; orphan reaping remains the operating system's job.

The scheduled command path is **preview-only**. Candidates preserve scoped
source references and remain subject to normal MemoryD validation and review.
Enabling automatic apply with this mode is rejected rather than treating model
output as accepted memory. Inspect native Dreamer/patch records before applying
any proposal. A successful provider request is not proof every candidate was
accepted or useful.

## Verification

```sh
python -m unittest discover -s tests -p test_dreamer_native_provider.py -v
cargo test --lib
cargo test --test dream_command_scheduler --test command_no_http_fallback --test command_job_policy --test config_env
```

The command tests are Linux-scoped. Use a disposable database for actual
subscription tests; `dream --scheduled` exercises the scheduler's configured
provider, not a separate shell smoke test. Verify `command` provenance, the
selected model, validated `dream_provider_` observations, and zero accepted
memory writes. Keep raw receipts and profile paths private.

Three `dream_jobs` fixture failures also reproduce on the unchanged base:
HTTP fixtures used in HTTPS-only Provider mode, and a configured-credential
endpoint mismatch. They are not waived security checks or a green full suite.
