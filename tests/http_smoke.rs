//! HTTP smoke tests: boot the axum server on an ephemeral port and exercise the
//! `/v1` endpoints over the wire, asserting the response envelope shape.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use codex_memoryd::config::Config;
use codex_memoryd::server::router;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::json;
use serde_json::Value;

/// Boot the server in a background tokio runtime thread and return its base URL.
fn boot() -> (String, std::thread::JoinHandle<()>) {
    boot_with_config(Config {
        bind: "127.0.0.1:0".to_string(),
        default_workspace: "josh-personal".to_string(),
        ..Default::default()
    })
}

fn boot_with_config(config: Config) -> (String, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let bind = config.bind.clone();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async move {
            let store = Store::open(":memory:").expect("store");
            let service = Service::new(store, config);
            let app = router(service);
            let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            let test_host = if addr.ip().is_unspecified() {
                "127.0.0.1".to_string()
            } else {
                addr.ip().to_string()
            };
            let public_host = match test_host.parse::<IpAddr>().unwrap() {
                IpAddr::V6(_) => format!("[{test_host}]"),
                IpAddr::V4(_) => test_host,
            };
            let base = format!("http://{public_host}:{}", addr.port());
            tx.send(base).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let base = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server addr");
    (base, handle)
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn raw_http_request_with_body_prefix(base: &str, request_head: &str, body_prefix: &str) -> String {
    let authority = base.strip_prefix("http://").unwrap();
    let (host, port) = authority.rsplit_once(':').unwrap();
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    let port: u16 = port.parse().unwrap();

    let mut stream = std::net::TcpStream::connect((host, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(request_head.as_bytes()).unwrap();
    stream.write_all(body_prefix.as_bytes()).unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[test]
fn http_status_recall_sync_roundtrip() {
    let (base, _handle) = boot();
    let http = client();

    // GET /v1/status
    let status: Value = http
        .get(format!("{base}/v1/status"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["data"]["provider_name"], json!("codex-memoryd"));
    assert_eq!(status["data"]["api_version"], json!("v1"));
    assert_eq!(status["data"]["status"], json!("local_only"));
    assert_eq!(status["data"]["features"]["exposure"], json!("local_only"));
    assert!(status["request_id"].as_str().unwrap().starts_with("req_"));

    // POST /v1/conclusions creates a record.
    let concl: Value = http
        .post(format!("{base}/v1/conclusions"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "target": "user",
            "conclusions": ["Decision: serve the provider with axum on 127.0.0.1:8787"]
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(concl["ok"], json!(true));
    assert_eq!(concl["data"]["created"].as_array().unwrap().len(), 1);

    // POST /v1/recall returns the fact.
    let recall: Value = http
        .post(format!("{base}/v1/recall"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "query": "how do we serve the provider"
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(recall["ok"], json!(true));
    assert_eq!(recall["data"]["authority"], json!("recall_not_authority"));
    let facts = recall["data"]["facts"].as_array().unwrap();
    assert!(!facts.is_empty());

    // POST /v1/sync/local-codex-memory preview writes nothing.
    let preview: Value = http
        .post(format!("{base}/v1/sync/local-codex-memory"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "repo": null,
            "source_root": "/home/u/.codex/memories",
            "mode": "preview",
            "files": [{
                "path": "memory_summary.md",
                "kind": "memory_summary",
                "content": "# Prefs\n- prefer repo-native workflows\n",
                "hash": "sha256:demo"
            }]
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(preview["ok"], json!(true));
    assert_eq!(preview["data"]["mode"], json!("preview"));
    assert_eq!(preview["data"]["created"], json!(0));
}

#[test]
fn http_subject_episode_roundtrip() {
    let (base, _handle) = boot();
    let http = client();

    let subject: Value = http
        .post(format!("{base}/v1/subjects"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "subject_key": "repo:codex-memoryd",
            "kind": "repo",
            "display_name": "codex-memoryd"
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(subject["ok"], json!(true));
    assert_eq!(subject["data"]["created"], json!(true));
    let subject_id = subject["data"]["subject"]["id"].as_str().unwrap();

    let duplicate: Value = http
        .post(format!("{base}/v1/subjects"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "subject_key": "repo:codex-memoryd",
            "kind": "repo",
            "display_name": "ignored duplicate"
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(duplicate["ok"], json!(true));
    assert_eq!(duplicate["data"]["created"], json!(false));
    assert_eq!(
        duplicate["data"]["subject"]["id"],
        subject["data"]["subject"]["id"]
    );

    let subjects: Value = http
        .get(format!(
            "{base}/v1/subjects?profile=personal&workspace=josh-personal&kind=repo"
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(subjects["ok"], json!(true));
    assert_eq!(subjects["data"]["subjects"].as_array().unwrap().len(), 1);

    let fetched_subject: Value = http
        .get(format!(
            "{base}/v1/subjects/get?profile=personal&workspace=josh-personal&id={subject_id}"
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(fetched_subject["ok"], json!(true));
    assert_eq!(
        fetched_subject["data"]["subject"]["subject_key"],
        json!("repo:codex-memoryd")
    );

    let episode: Value = http
        .post(format!("{base}/v1/episodes"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "subject_id": subject_id,
            "source_kind": "github_issue",
            "source_ref": "joshyorko/codex-memoryd#65",
            "summary": "Subject and episode storage MVP",
            "status": "open"
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(episode["ok"], json!(true));
    let episode_id = episode["data"]["episode"]["id"].as_str().unwrap();

    let episodes: Value = http
        .get(format!(
            "{base}/v1/episodes?profile=personal&workspace=josh-personal&subject_id={subject_id}"
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(episodes["ok"], json!(true));
    assert_eq!(episodes["data"]["episodes"].as_array().unwrap().len(), 1);

    let fetched_episode: Value = http
        .get(format!(
            "{base}/v1/episodes/get?profile=personal&workspace=josh-personal&id={episode_id}"
        ))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(fetched_episode["ok"], json!(true));
    assert_eq!(
        fetched_episode["data"]["episode"]["subject_id"],
        json!(subject_id)
    );
}

#[test]
fn http_status_reports_auth_missing_for_non_loopback_config() {
    let (base, _handle) = boot_with_config(Config {
        bind: "0.0.0.0:0".to_string(),
        default_workspace: "josh-personal".to_string(),
        ..Default::default()
    });
    let http = client();

    let status: Value = http
        .get(format!("{base}/v1/status"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(status["ok"], json!(true));
    assert_eq!(status["data"]["status"], json!("auth_missing"));
    assert_eq!(status["data"]["features"]["auth"], json!("none"));
    assert!(status["warnings"].as_array().unwrap().iter().any(|w| w
        .as_str()
        .unwrap()
        .contains("remote /v1 exposure is unsupported")));
}

#[test]
fn auth_missing_blocks_v1_routes_except_status() {
    let (base, _handle) = boot_with_config(Config {
        bind: "0.0.0.0:0".to_string(),
        default_workspace: "josh-personal".to_string(),
        ..Default::default()
    });
    let http = client();

    let health = http.get(format!("{base}/healthz")).send().unwrap();
    assert!(health.status().is_success());

    let status: Value = http
        .get(format!("{base}/v1/status"))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(status["data"]["status"], json!("auth_missing"));

    let assert_v1_blocked = |resp: reqwest::blocking::Response| {
        let status = resp.status().as_u16();
        assert!(status >= 400);
        let body: Value = resp.json().unwrap();
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["error"]["code"], json!("auth_missing"));
    };

    assert_v1_blocked(
        http.post(format!("{base}/v1/conclusions"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "target": "user",
                    "conclusions": ["Block this non-loopback endpoint"]
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/forget"))
            .json(&json!({
                "profile": "personal",
                "workspace": "josh-personal"
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/dream"))
            .json(&json!({
                "profile": "personal",
                "workspace": "josh-personal"
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/recall"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "query": "some query"
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/search"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "query": "some query"
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/turns"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "session": { "id": "s1", "source": "test" },
                    "messages": []
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/checkpoints"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "summary": "No-op checkpoint"
            }))
            .send()
            .unwrap(),
    );

    assert_v1_blocked(
        http.post(format!("{base}/v1/sync/local-codex-memory"))
            .json(&json!({
                    "profile": "personal",
                    "workspace": "josh-personal",
                    "source_root": "/home/u/.codex/memories",
                    "mode": "preview",
                "files": []
            }))
            .send()
            .unwrap(),
    );

    let export_resp = http
        .get(format!(
            "{base}/v1/export?profile=personal&workspace=josh-personal"
        ))
        .send()
        .unwrap();
    let export_headers = export_resp.headers().clone();
    let export_body = export_resp.text().unwrap();
    assert_eq!(export_headers.get("x-record-count"), None, "{export_body}");
    let export_body: Value = serde_json::from_str(&export_body).unwrap();
    assert_eq!(export_body["ok"], json!(false));
    assert_eq!(export_body["error"]["code"], json!("auth_missing"));

    let raw_export = raw_http_request_with_body_prefix(
        &base,
        "POST /v1/recall HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 1048576\r\n\
         Connection: close\r\n\
         \r\n",
        "{",
    );
    let status_line = raw_export.lines().next().unwrap_or_default();
    assert!(status_line.contains("401"), "{raw_export}");
    assert!(
        raw_export.contains("\"code\":\"auth_missing\""),
        "{raw_export}"
    );
    assert!(!raw_export.contains("invalid JSON body"), "{raw_export}");
}

#[test]
fn http_error_bodies_are_bounded_and_do_not_echo_rejected_content() {
    let (base, _handle) = boot();
    let http = client();

    let bad_json = "not-json raw-rejected-content";
    let resp = http
        .post(format!("{base}/v1/recall"))
        .header("content-type", "application/json")
        .body(bad_json)
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let body = resp.text().unwrap();
    assert!(body.len() < 512);
    assert!(!body.contains("raw-rejected-content"));
    assert!(body.contains("invalid JSON body"));

    let bad_actor = "raw-rejected-actor";
    let invalid_actor: Value = http
        .post(format!("{base}/v1/turns"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "session": { "id": "s1", "source": "test" },
            "messages": [
                { "actor": bad_actor, "content": "hello" }
            ]
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(invalid_actor["ok"], json!(true));
    assert_eq!(invalid_actor["data"]["rejected"], json!(1));
    assert!(!invalid_actor.to_string().contains(bad_actor));
}

#[test]
fn http_secret_is_rejected_on_turns() {
    let (base, _handle) = boot();
    let http = client();
    let resp: Value = http
        .post(format!("{base}/v1/turns"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "session": { "id": "s1", "source": "test" },
            "messages": [
                { "actor": "user", "content": "token ghp_abcdefghijklmnopqrstuvwxyz0123456789" }
            ]
        }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(resp["ok"], json!(true));
    assert_eq!(resp["data"]["accepted"], json!(0));
    assert_eq!(resp["data"]["rejected"], json!(1));
}

#[test]
fn http_checkpoint_endpoint_exists() {
    let (base, _handle) = boot();
    let http = client();
    let resp = http
        .post(format!("{base}/v1/checkpoints"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "summary": "Boot the HTTP server and verify endpoints"
        }))
        .send()
        .unwrap();
    assert!(resp.status().is_success());
    let body: Value = resp.json().unwrap();
    assert_eq!(body["ok"], json!(true));
    assert!(body["data"]["id"].as_str().unwrap().starts_with("ckpt_"));
}

#[test]
fn http_export_streams_jsonl() {
    let (base, _handle) = boot();
    let http = client();
    // Seed a record.
    http.post(format!("{base}/v1/conclusions"))
        .json(&json!({
            "profile": "personal",
            "workspace": "josh-personal",
            "target": "user",
            "conclusions": ["I prefer concise commit messages"]
        }))
        .send()
        .unwrap();

    let resp = http
        .get(format!(
            "{base}/v1/export?profile=personal&workspace=josh-personal"
        ))
        .send()
        .unwrap();
    assert!(resp.status().is_success());
    let count = resp
        .headers()
        .get("x-record-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(count >= 1);
    let body = resp.text().unwrap();
    assert!(body.contains("concise commit messages"));
}

#[test]
fn http_export_denies_work_to_personal() {
    let (base, _handle) = boot();
    let http = client();
    http.post(format!("{base}/v1/conclusions"))
        .json(&json!({
            "profile": "work",
            "workspace": "acme",
            "target": "user",
            "conclusions": ["Internal deployment runbook decision"]
        }))
        .send()
        .unwrap();

    let resp = http
        .get(format!(
            "{base}/v1/export?profile=work&workspace=acme&target_profile=personal"
        ))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 422);
    let body: Value = resp.json().unwrap();
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["error"]["code"], json!("profile_boundary_denied"));
}
