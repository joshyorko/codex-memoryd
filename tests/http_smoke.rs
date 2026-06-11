//! HTTP smoke tests: boot the axum server on an ephemeral port and exercise the
//! `/v1` endpoints over the wire, asserting the response envelope shape.

use std::net::SocketAddr;
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
        default_workspace: "josh-personal".to_string(),
        ..Default::default()
    })
}

fn boot_with_config(config: Config) -> (String, std::thread::JoinHandle<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        rt.block_on(async move {
            let store = Store::open(":memory:").expect("store");
            let service = Service::new(store, config);
            let app = router(service);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr: SocketAddr = listener.local_addr().unwrap();
            tx.send(addr).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("server addr");
    (format!("http://{addr}"), handle)
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
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
fn http_status_reports_auth_missing_for_non_loopback_config() {
    let (base, _handle) = boot_with_config(Config {
        bind: "0.0.0.0:8787".to_string(),
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
