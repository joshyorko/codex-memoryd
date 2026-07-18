use anyhow::Result;
use reqwest::blocking::Client;
use serde_json::json;
use serde_json::Value;

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

#[cfg(test)]
mod tests {
    use super::generate_observations;
    use std::io::{Read, Write};
    use std::net::TcpListener;

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
}
