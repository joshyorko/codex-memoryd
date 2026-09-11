use anyhow::Result;
use reqwest::blocking::Client;
use serde_json::json;
use serde_json::Value;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use std::time::Instant;

use crate::error::Error;
use crate::ids;
use crate::protocol::DreamBudgetUsage;
use crate::protocol::DreamJobBudget;
use crate::protocol::DreamProviderAdapter;

const SYSTEM_PROMPT: &str = r#"Extract durable memory observations from the supplied evidence.
Return only a JSON array. Each item must match the codex-memoryd dream observation format and
represent a useful preference, gotcha, recurring pattern, or task. Include all required fields:
id, key, kind, category, subject_key, summary, content, confidence, state, evidence_refs, retires,
counter_evidence_refs, first_seen_at, last_seen_at, authority, policy, and apply_eligible.
Use kind "dream_observation", authority "recall_not_authority", policy "provider_generated",
and apply_eligible false. Do not invent facts absent from the evidence."#;

pub fn generate_observations(
    endpoint: &str,
    api_key: &str,
    model: &str,
    evidence_context: &str,
) -> Result<Vec<Value>> {
    let url = format!("{}/chat/completions", endpoint.trim_end_matches('/'));
    let mut request = Client::new().post(url).json(&json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": evidence_context}
        ],
        "temperature": 0.7,
        "max_tokens": 2048
    }));
    if !api_key.trim().is_empty() {
        request = request.bearer_auth(api_key);
    }

    let observations = request
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::json::<Value>)
        .ok()
        .and_then(|response| {
            response
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(|content| {
                    content
                        .strip_prefix("```json")
                        .or_else(|| content.strip_prefix("```"))
                        .unwrap_or(content)
                        .strip_suffix("```")
                        .unwrap_or(content)
                        .trim()
                        .to_string()
                })
        })
        .and_then(|content| serde_json::from_str::<Value>(&content).ok())
        .and_then(|value| match value {
            Value::Array(values) => Some(values),
            Value::Object(mut object) => object
                .remove("observations")
                .and_then(|value| value.as_array().cloned()),
            _ => None,
        })
        .unwrap_or_default();

    Ok(observations)
}

pub const DREAM_PROVIDER_SCHEMA_VERSION: &str = "dream-preview-v1";
pub const DREAM_PROVIDER_ADAPTER_VERSION: &str = "http-json-v1";
pub const DREAM_COMMAND_ADAPTER_VERSION: &str = "native-command-v1";

/// Runtime-only provider call configuration. It deliberately contains the
/// credential only in memory; callers must never persist or serialize it.
pub struct DreamProviderRequest<'a> {
    pub adapter: DreamProviderAdapter,
    pub endpoint: &'a str,
    pub command: &'a [String],
    pub api_key: &'a str,
    pub model: &'a str,
    pub provider_name: &'a str,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_provider_calls: usize,
    pub max_retries: usize,
    pub deadline: Instant,
}

#[derive(Debug, Clone)]
pub struct DreamProviderCall {
    pub values: Vec<Value>,
    pub response_profile: Option<String>,
    pub response_workspace: Option<String>,
    pub response_repo_id: Option<String>,
    pub response_repo_id_present: bool,
    pub usage: DreamBudgetUsage,
    pub reported_cost_micros: Option<u64>,
    pub request_hash: String,
    pub input_hash: String,
}

/// Execute the reviewed HTTP JSON adapter. The adapter accepts either the
/// typed Dream preview envelope or an OpenAI-compatible chat response whose
/// content contains that envelope. No subprocess or shell execution is
/// involved.
pub fn execute_preview(
    request: &DreamProviderRequest<'_>,
    profile: &str,
    workspace: &str,
    repo_id: Option<&str>,
    model_input: &str,
    budget: &DreamJobBudget,
) -> crate::error::Result<DreamProviderCall> {
    if !request.adapter.is_model_backed() {
        return Err(Error::invalid_request(
            "deterministic jobs must not invoke a model adapter",
        ));
    }
    if request.adapter != DreamProviderAdapter::Command && request.endpoint.trim().is_empty() {
        return Err(Error::invalid_request(
            "model-backed Dream jobs require an explicit provider endpoint",
        ));
    }
    if request.max_response_bytes == 0 {
        return Err(Error::invalid_request(
            "dream provider max response bytes must be > 0",
        ));
    }
    if request.max_provider_calls == 0 {
        return Err(Error::invalid_request(
            "dream provider max_provider_calls must be > 0",
        ));
    }

    let input_bytes = model_input.as_bytes().len();
    let input_tokens = estimate_tokens(model_input);
    let request_body = json!({
        "schema_version": DREAM_PROVIDER_SCHEMA_VERSION,
        "profile": profile,
        "workspace": workspace,
        "repo_id": repo_id,
        "model": request.model,
        "input": model_input,
        "response_schema": {
            "type": "object", "additionalProperties": false,
            "required": ["schema_version", "profile", "workspace", "repo_id", "candidates"],
            "properties": {
                "schema_version": {"const": DREAM_PROVIDER_SCHEMA_VERSION},
                "profile": {"const": profile}, "workspace": {"const": workspace},
                "repo_id": {"const": repo_id},
                "candidates": {"type": "array", "maxItems": budget.max_candidates,
                    "items": {"type": "object", "additionalProperties": false,
                        "required": ["content", "type", "subject_key", "evidence_refs", "confidence"],
                        "properties": {
                            "content": {"type": "string", "minLength": 1},
                            "type": {"type": "string", "description": "MemoryD type such as preference, decision, repo_convention or workflow_pattern"},
                            "subject_key": {"type": "string", "minLength": 1},
                            "evidence_refs": {"type": "array", "minItems": 1, "items": {"type": "string"}, "description": "Exact IDs from supplied evidence_content; never invented IDs"},
                            "confidence": {"type": "number", "minimum": 0, "maximum": 1}
                        }
                    }
                }
            }
        }
    });
    let request_bytes = serde_json::to_vec(&request_body)
        .map_err(|_| Error::internal("provider request could not be encoded"))?;
    if budget.max_input_bytes > 0 && input_bytes > budget.max_input_bytes {
        return Err(Error::internal(
            "dream provider input byte budget exhausted",
        ));
    }
    if budget.max_input_tokens > 0 && input_tokens > budget.max_input_tokens {
        return Err(Error::internal(
            "dream provider input token budget exhausted",
        ));
    }

    let request_hash = ids::sha256_hex(&request_bytes);
    let input_hash = ids::sha256_hex(model_input.as_bytes());
    let max_calls = request.max_provider_calls;
    let max_attempts = max_calls.min(request.max_retries.saturating_add(1));
    let mut last_error = "provider request failed";

    for attempt in 0..max_attempts {
        if Instant::now() >= request.deadline {
            return Err(Error::internal("dream provider runtime budget exhausted"));
        }
        let remaining = request
            .deadline
            .saturating_duration_since(Instant::now())
            .min(request.timeout);
        if remaining.is_zero() {
            return Err(Error::internal("dream provider runtime budget exhausted"));
        }
        match call_once(
            request,
            &request_body,
            remaining,
            if budget.max_output_bytes == 0 {
                request.max_response_bytes
            } else {
                request.max_response_bytes.min(budget.max_output_bytes)
            },
        ) {
            Ok((value, output_bytes, reported_cost_micros)) => {
                let extracted = extract_response(value)?;
                let output_tokens = extracted
                    .usage
                    .completion_tokens
                    .unwrap_or_else(|| estimate_tokens_from_bytes(output_bytes));
                let input_tokens = extracted.usage.prompt_tokens.unwrap_or(input_tokens);
                let usage = DreamBudgetUsage {
                    input_records: 0,
                    input_tokens,
                    input_bytes,
                    output_candidates: extracted.values.len(),
                    output_tokens,
                    output_bytes,
                    provider_calls: attempt + 1,
                    retries: attempt,
                    cost_micros: reported_cost_micros.unwrap_or(0),
                };
                return Ok(DreamProviderCall {
                    values: extracted.values,
                    response_profile: extracted.profile,
                    response_workspace: extracted.workspace,
                    response_repo_id: extracted.repo_id,
                    response_repo_id_present: extracted.repo_id_present,
                    usage,
                    reported_cost_micros,
                    request_hash,
                    input_hash,
                });
            }
            Err(error) => {
                last_error = error;
            }
        }
    }

    if max_attempts < request.max_provider_calls && request.max_retries > 0 {
        return Err(Error::internal("dream provider retry budget exhausted"));
    }
    Err(Error::internal(last_error))
}

struct ExtractedResponse {
    values: Vec<Value>,
    profile: Option<String>,
    workspace: Option<String>,
    repo_id: Option<String>,
    repo_id_present: bool,
    usage: ParsedUsage,
}

#[derive(Default)]
struct ParsedUsage {
    prompt_tokens: Option<usize>,
    completion_tokens: Option<usize>,
}

fn call_once(
    request: &DreamProviderRequest<'_>,
    body: &Value,
    timeout: Duration,
    max_response_bytes: usize,
) -> std::result::Result<(Value, usize, Option<u64>), &'static str> {
    if request.adapter == DreamProviderAdapter::Command {
        return call_command(request.command, body, timeout, max_response_bytes);
    }
    let endpoint = request.endpoint.trim_end_matches('/');
    let url = if endpoint.ends_with("/chat/completions") {
        endpoint.to_string()
    } else {
        format!("{endpoint}/chat/completions")
    };
    let client = Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|_| "provider client unavailable")?;
    let mut call = client.post(url).json(body);
    if !request.api_key.trim().is_empty() {
        call = call.bearer_auth(request.api_key);
    }
    let response = call.send().map_err(|error| {
        if error.is_timeout() {
            "provider request timed out"
        } else {
            "provider request failed"
        }
    })?;
    if !response.status().is_success() {
        return Err("provider returned an error status");
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_response_bytes as u64)
    {
        return Err("provider output byte budget exhausted");
    }
    let read_limit = (max_response_bytes as u64).saturating_add(1);
    let mut bytes = Vec::with_capacity(max_response_bytes.min(8192));
    response
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| "provider response could not be read")?;
    if bytes.len() > max_response_bytes {
        return Err("provider output byte budget exhausted");
    }
    let value = serde_json::from_slice::<Value>(&bytes)
        .map_err(|_| "provider response was not valid JSON")?;
    let reported_cost_micros = value
        .get("cost_micros")
        .and_then(Value::as_u64)
        .or_else(|| value.pointer("/usage/cost_micros").and_then(Value::as_u64));
    Ok((value, bytes.len(), reported_cost_micros))
}

fn call_command(
    argv: &[String],
    body: &Value,
    timeout: Duration,
    max_response_bytes: usize,
) -> std::result::Result<(Value, usize, Option<u64>), &'static str> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (argv, body, timeout, max_response_bytes);
        return Err("command providers require Linux");
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::ErrorKind;
        use std::os::fd::AsRawFd;
        if argv.is_empty() || argv[0].trim().is_empty() || argv.iter().any(|arg| arg.contains('\0'))
        {
            return Err("provider command is invalid");
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or("provider command timed out")?;
        let input =
            serde_json::to_vec(body).map_err(|_| "provider request could not be encoded")?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_process_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|_| "provider command could not start")?;
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take();
        let result = (|| {
            let fds = [
                stdin
                    .as_ref()
                    .ok_or("provider command stdin unavailable")?
                    .as_raw_fd(),
                stdout
                    .as_ref()
                    .ok_or("provider command stdout unavailable")?
                    .as_raw_fd(),
            ];
            for fd in fds {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                {
                    return Err("provider command pipe setup failed");
                }
            }
            let mut offset = 0;
            let mut bytes = Vec::with_capacity(max_response_bytes.min(8192));
            let mut eof = false;
            let mut status = None;
            loop {
                if Instant::now() >= deadline {
                    return Err("provider command timed out");
                }
                if let Some(pipe) = stdin.as_mut() {
                    match pipe.write(&input[offset..]) {
                        Ok(0) if offset < input.len() => {
                            return Err("provider command stdin failed")
                        }
                        Ok(n) => offset += n,
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::WouldBlock | ErrorKind::Interrupted
                            ) => {}
                        Err(_) => return Err("provider command stdin failed"),
                    }
                    if offset == input.len() {
                        stdin.take();
                    }
                }
                if !eof {
                    let mut buffer = [0u8; 8192];
                    let remaining = max_response_bytes
                        .saturating_add(1)
                        .saturating_sub(bytes.len())
                        .min(buffer.len());
                    match stdout.as_mut().unwrap().read(&mut buffer[..remaining]) {
                        Ok(0) => eof = true,
                        Ok(n) => {
                            bytes.extend_from_slice(&buffer[..n]);
                            if bytes.len() > max_response_bytes {
                                return Err("provider output byte budget exhausted");
                            }
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                ErrorKind::WouldBlock | ErrorKind::Interrupted
                            ) => {}
                        Err(_) => return Err("provider command stdout failed"),
                    }
                }
                if status.is_none() {
                    status = child
                        .try_wait()
                        .map_err(|_| "provider command wait failed")?;
                }
                if let Some(exit) = status {
                    if !exit.success() {
                        return Err("provider command exited unsuccessfully");
                    }
                    if eof && stdin.is_none() {
                        let value = serde_json::from_slice::<Value>(&bytes)
                            .map_err(|_| "provider response was not valid JSON")?;
                        let reported_cost_micros = value
                            .get("cost_micros")
                            .and_then(Value::as_u64)
                            .or_else(|| {
                                value.pointer("/usage/cost_micros").and_then(Value::as_u64)
                            });
                        return Ok((value, bytes.len(), reported_cost_micros));
                    }
                }
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(1)),
                );
            }
        })();
        drop(stdin);
        drop(stdout);
        terminate_process_tree(&mut child);
        result
    }
}

fn configure_process_group(command: &mut Command) {
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(target_os = "linux")]
    {
        if let Ok(pid) = libc::pid_t::try_from(child.id()) {
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn extract_response(value: Value) -> crate::error::Result<ExtractedResponse> {
    let mut payload = value.clone();
    let mut usage = parsed_usage(&value);
    if let Some(content) = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        let content = content
            .trim()
            .strip_prefix("```json")
            .or_else(|| content.trim().strip_prefix("```"))
            .unwrap_or(content.trim())
            .strip_suffix("```")
            .unwrap_or(content.trim())
            .trim();
        payload = serde_json::from_str(content)
            .map_err(|_| Error::internal("provider response content was not valid JSON"))?;
        usage = parsed_usage(&value);
    }

    let schema_version = payload
        .get("schema_version")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::internal("provider response missing schema version"))?;
    if schema_version != DREAM_PROVIDER_SCHEMA_VERSION {
        return Err(Error::new(
            crate::error::ErrorCode::UnsupportedVersion,
            "provider response schema version is unsupported",
        ));
    }

    let values = match &payload {
        Value::Object(object) => object
            .get("candidates")
            .or_else(|| object.get("observations"))
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| Error::internal("provider response had no candidate array"))?,
        _ => return Err(Error::internal("provider response had an invalid schema")),
    };
    let profile = match payload.get("profile") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(Error::internal("provider response had an invalid profile")),
    };
    let workspace = match payload.get("workspace") {
        None => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => {
            return Err(Error::internal(
                "provider response had an invalid workspace",
            ))
        }
    };
    let repo_id_present = payload
        .as_object()
        .is_some_and(|object| object.contains_key("repo_id"));
    let repo_id = match payload.get("repo_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(Error::internal("provider response had an invalid repo_id")),
    };
    Ok(ExtractedResponse {
        values,
        profile,
        workspace,
        repo_id,
        repo_id_present,
        usage,
    })
}

fn parsed_usage(value: &Value) -> ParsedUsage {
    ParsedUsage {
        prompt_tokens: value
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
        completion_tokens: value
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    }
}

fn estimate_tokens(value: &str) -> usize {
    estimate_tokens_from_bytes(value.len())
}

fn estimate_tokens_from_bytes(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

#[cfg(test)]
mod tests {
    use super::generate_observations;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    #[test]
    fn generates_observations_from_openai_compatible_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let size = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key"));
            assert!(request.contains("test-model"));
            assert!(request.contains("Evidence text"));

            let content = serde_json::json!([{"kind": "dream_observation", "content": "Prefer concise output"}]).to_string();
            let body = serde_json::json!({
                "choices": [{"message": {"content": content}}]
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });

        let endpoint = format!("http://{address}/v1");
        let observations =
            generate_observations(&endpoint, "test-key", "test-model", "Evidence text")
                .expect("provider call");
        server.join().expect("test server");

        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["content"], "Prefer concise output");
    }

    #[test]
    fn provider_failure_returns_empty_observations() {
        let observations =
            generate_observations("http://127.0.0.1:1/v1", "", "test-model", "Evidence text")
                .expect("fail open");
        assert!(observations.is_empty());
    }

    fn command_run(script: &str, limit: usize) -> Result<serde_json::Value, &'static str> {
        let argv = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        super::call_command(
            &argv,
            &json!({"input": "synthetic"}),
            Duration::from_millis(250),
            limit,
        )
        .map(|(value, _, _)| value)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_returns_json_and_rejects_bad_outputs() {
        assert_eq!(
            command_run(r#"cat >/dev/null; printf '{"ok":true}'"#, 100).unwrap(),
            json!({"ok": true})
        );
        assert_eq!(
            command_run("cat >/dev/null; printf '123456789'", 4),
            Err("provider output byte budget exhausted")
        );
        assert_eq!(
            command_run("cat >/dev/null; printf '{'", 100),
            Err("provider response was not valid JSON")
        );
        assert_eq!(
            command_run("cat >/dev/null; exit 7", 100),
            Err("provider command exited unsuccessfully")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_job_byte_limit_stops_the_reader_before_process_exit() {
        let argv = vec![
            "/bin/sh".into(),
            "-c".into(),
            "cat >/dev/null; printf 123456789; sleep 2".into(),
        ];
        let request = super::DreamProviderRequest {
            adapter: crate::protocol::DreamProviderAdapter::Command,
            endpoint: "",
            command: &argv,
            api_key: "",
            model: "synthetic",
            provider_name: "synthetic",
            timeout: Duration::from_millis(250),
            max_response_bytes: 1024,
            max_provider_calls: 1,
            max_retries: 0,
            deadline: std::time::Instant::now() + Duration::from_secs(1),
        };
        let budget = crate::protocol::DreamJobBudget {
            max_output_bytes: 4,
            ..Default::default()
        };
        let err = super::execute_preview(&request, "personal", "ws", None, "synthetic", &budget)
            .unwrap_err();
        assert!(err.message.contains("output byte budget"), "{err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn blocked_large_stdin_obeys_deadline_without_pipe_threads() {
        let argv = vec!["/bin/sh".into(), "-c".into(), "sleep 2".into()];
        let result = super::call_command(
            &argv,
            &json!({"input": "x".repeat(131072)}),
            Duration::from_millis(100),
            100,
        );
        assert!(matches!(result, Err("provider command timed out")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_deadline_covers_hung_stdin() {
        assert_eq!(
            command_run("sleep 5", 100),
            Err("provider command timed out")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn command_kills_descendants_on_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        let script = format!(
            "cat >/dev/null; (sleep 5) & echo $! > {}; wait",
            pid_file.display()
        );
        assert_eq!(command_run(&script, 100), Err("provider command timed out"));
        let pid = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        // A killed grandchild may await its OS parent/init reaper; it must not run.
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            let state = stat.rsplit_once(") ").unwrap().1.chars().next().unwrap();
            assert!(
                matches!(state, 'Z' | 'X'),
                "descendant still active: {state}"
            );
        }
    }
}
