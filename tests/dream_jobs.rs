use codex_memoryd::config::Config;
use codex_memoryd::domain::{Portability, RecordType, RepoIdentity, Scope, Sensitivity};
use codex_memoryd::ids;
use codex_memoryd::protocol::{
    CheckpointRequest, ConclusionsRequest, DreamJobBudget, DreamJobProvider, DreamJobRunRequest,
    DreamProviderAdapter, DreamProviderCommand,
};
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store};
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread::JoinHandle;

fn service() -> Service {
    let store = Store::open(":memory:").expect("open store");
    let config = Config {
        default_workspace: "ws".to_string(),
        ..Default::default()
    };
    Service::new(store, config)
}

fn conclude(svc: &Service, content: &str) {
    svc.conclusions(ConclusionsRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        target: Some("user".to_string()),
        conclusions: Some(vec![content.to_string()]),
        metadata: None,
        record_type: None,
    })
    .unwrap();
}

fn checkpoint(svc: &Service, summary: &str) {
    svc.checkpoint(CheckpointRequest {
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None,
        session: None,
        summary: Some(summary.to_string()),
        changed_files: vec![],
        decisions: vec![],
        blockers: vec![],
        next_steps: vec![],
        tests_run: vec![],
        tests_not_run: vec![],
        branch: None,
        commit: None,
    })
    .unwrap();
}

fn base_request() -> DreamJobRunRequest {
    DreamJobRunRequest {
        job_id: Some("job_default".to_string()),
        profile: Some("personal".to_string()),
        workspace: Some("ws".to_string()),
        repo: None::<RepoIdentity>,
        now: Some("2030-01-01T00:00:00Z".to_string()),
        since: None,
        kind: "dream_preview".to_string(),
        mode: Some("deterministic".to_string()),
        budget: DreamJobBudget {
            max_runtime_seconds: 30,
            max_input_records: 500,
            max_candidates: 5,
            ..Default::default()
        },
        provider: None,
    }
}

fn fake_provider() -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider");
    let address = listener.local_addr().expect("provider address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider request");
        let mut request = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = stream.read(&mut chunk).expect("read provider request");
            request.extend_from_slice(&chunk[..read]);
            if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        lower.strip_prefix("content-length:").map(str::to_string)
                    })
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    let body = &request[header_end + 4..header_end + 4 + content_length];
                    let body: serde_json::Value =
                        serde_json::from_slice(body).expect("provider request json");
                    let input: serde_json::Value =
                        serde_json::from_str(body["input"].as_str().expect("provider input"))
                            .expect("provider input json");
                    let source_id = input["evidence_window"]
                        .as_object()
                        .into_iter()
                        .flat_map(|streams| streams.values())
                        .filter_map(|stream| stream.get("sources"))
                        .flat_map(|sources| sources.as_array().into_iter().flatten())
                        .find_map(|source| source.get("id").and_then(|id| id.as_str()))
                        .expect("provider request evidence id");
                    let response = json!({
                        "schema_version": "dream-preview-v1",
                        "profile": body["profile"],
                        "workspace": body["workspace"],
                        "repo_id": body["repo_id"],
                        "candidates": [{
                            "type": "preference",
                            "content": "Use concise commit messages.",
                            "subject_key": "commit-style",
                            "confidence": 0.82,
                            "evidence_refs": [source_id]
                        }]
                    })
                    .to_string();
                    write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response.len(),
                            response
                        )
                        .expect("write provider response");
                    break;
                }
            }
        }
    });
    (format!("http://{address}/v1"), server)
}

#[test]
fn deterministic_job_run_is_preview_only_and_persists_budgeted_job_record() {
    let svc = service();
    conclude(
        &svc,
        "Right now the daemon is failing on startup, planning to patch it tomorrow.",
    );
    conclude(&svc, "OAuth sync is planned; will implement it next week.");
    std::thread::sleep(std::time::Duration::from_millis(5));
    checkpoint(&svc, "Implemented OAuth sync and merged it.");
    let before = svc.store.count_records().unwrap();

    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_det_preview".to_string()),
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None::<RepoIdentity>,
            now: Some("2030-01-01T00:00:00Z".to_string()),
            since: None,
            kind: "dream_preview".to_string(),
            mode: Some("deterministic".to_string()),
            budget: DreamJobBudget {
                max_runtime_seconds: 30,
                max_input_records: 500,
                max_candidates: 5,
                ..Default::default()
            },
            provider: Some(DreamJobProvider {
                command: Some(DreamProviderCommand {
                    argv: vec!["/bin/false".to_string(), "--never-run".to_string()],
                }),
                ..Default::default()
            }),
        })
        .unwrap();

    assert_eq!(run.status, "ok");
    assert_eq!(run.mode, "preview");
    assert_eq!(run.preview.mode, "preview");
    assert!(!run.preview.candidates.is_empty());
    assert_eq!(svc.store.count_records().unwrap(), before);
    assert_eq!(svc.store.count_table_rows("dream_jobs").unwrap(), 1);

    let job = svc.store.get_dream_job("job_det_preview").unwrap().unwrap();
    assert_eq!(job.kind, "dream_preview");
    assert_eq!(job.mode, "deterministic");
    assert_eq!(job.budget.max_candidates, 5);
    assert_eq!(
        job.provider.command.unwrap().argv,
        vec!["/bin/false".to_string(), "--never-run".to_string()]
    );
    assert_eq!(job.last_run_id.as_deref(), Some(run.run_id.as_str()));
}

#[test]
fn deterministic_job_run_reuses_dream_run_audit_and_enforces_candidate_budget() {
    let svc = service();
    conclude(
        &svc,
        "Right now the daemon is failing on startup, planning to patch it tomorrow.",
    );
    conclude(&svc, "OAuth sync is planned; will implement it next week.");
    std::thread::sleep(std::time::Duration::from_millis(5));
    checkpoint(&svc, "Implemented OAuth sync and merged it.");

    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_limit".to_string()),
            profile: Some("personal".to_string()),
            workspace: Some("ws".to_string()),
            repo: None::<RepoIdentity>,
            now: Some("2030-01-01T00:00:00Z".to_string()),
            since: None,
            kind: "dream_preview".to_string(),
            mode: Some("deterministic".to_string()),
            budget: DreamJobBudget {
                max_runtime_seconds: 30,
                max_input_records: 500,
                max_candidates: 1,
                ..Default::default()
            },
            provider: None,
        })
        .unwrap();

    assert_eq!(run.status, "ok_with_limits");
    assert!(run.limits_hit.contains(&"max_candidates".to_string()));
    assert!(run.preview.candidates.len() <= 1);

    let last = svc.store.last_dream_run().unwrap().unwrap();
    assert_eq!(last.id, run.run_id);
    assert_eq!(last.mode, "preview");
}

#[test]
fn deterministic_job_run_budgets_policy_rejections_as_candidate_outcomes() {
    let svc = service();
    for (index, content) in [
        "Right now alpha deployment secret=alpha-secret-123456",
        "Right now beta deployment secret=beta-secret-123456",
        "Right now gamma deployment secret=gamma-secret-123456",
    ]
    .into_iter()
    .enumerate()
    {
        svc.store
            .upsert_record(&NewRecord {
                profile_id: "personal".to_string(),
                workspace_id: "ws".to_string(),
                repo_id: None,
                subject_id: None,
                episode_id: None,
                scope: Scope::Session,
                record_type: RecordType::Decision,
                content: content.to_string(),
                related_files: vec![],
                tags: vec![],
                sensitivity: Sensitivity::Personal,
                portability: Portability::ProfileOnly,
                confidence: 0.9,
                source_ids: vec![],
                content_hash: ids::content_hash(
                    "personal",
                    "ws",
                    None,
                    RecordType::Decision.as_str(),
                    Scope::Session.as_str(),
                    content,
                ),
                supersedes: vec![],
                metadata: json!({
                    "subject_key": format!("deployment-{index}"),
                }),
            })
            .expect("test record should be inserted");
    }

    let mut req = base_request();
    req.job_id = Some("job_rejection_limit".to_string());
    req.budget.max_candidates = 1;
    let run = svc
        .run_dream_job(req)
        .expect("dream job should return policy rejections");
    let outcome_count = run.preview.candidates.len() + run.preview.rejected.len();

    assert!(
        outcome_count <= 1,
        "candidate outcome budget exceeded: candidates={}, rejected={}, limits_hit={:?}",
        run.preview.candidates.len(),
        run.preview.rejected.len(),
        run.limits_hit
    );
    assert!(
        run.limits_hit.contains(&"max_candidates".to_string()),
        "max_candidates limit missing for candidates={}, rejected={}, status={}",
        run.preview.candidates.len(),
        run.preview.rejected.len(),
        run.status
    );
    assert_eq!(run.status, "ok_with_limits");
}

#[test]
fn deterministic_job_run_rejects_zero_runtime_budget() {
    let svc = service();

    let mut req = base_request();
    req.job_id = Some("job_zero_runtime".to_string());
    req.budget.max_runtime_seconds = 0;

    let err = svc
        .run_dream_job(req)
        .expect_err("zero runtime budget should be rejected");

    assert!(
        err.message.contains("max_runtime_seconds must be > 0"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn deterministic_job_run_rejects_oversized_runtime_budget() {
    let svc = service();

    let mut req = base_request();
    req.job_id = Some("job_oversized_runtime".to_string());
    req.budget.max_runtime_seconds = u64::MAX;

    let err = svc
        .run_dream_job(req)
        .expect_err("oversized runtime budget should be rejected");

    assert!(
        err.message.contains("max_runtime_seconds is too large"),
        "unexpected error: {err:?}"
    );
    assert_eq!(svc.store.count_table_rows("dream_jobs").unwrap(), 0);
}

#[test]
fn deterministic_job_run_rejects_invalid_mode_kind_and_timestamps() {
    let svc = service();

    let mut bad_mode = base_request();
    bad_mode.job_id = Some("job_bad_mode".to_string());
    bad_mode.mode = Some("model".to_string());
    let mode_err = svc
        .run_dream_job(bad_mode)
        .expect_err("non-deterministic mode should be rejected");
    assert!(mode_err.message.contains("mode must be deterministic"));

    let mut bad_kind = base_request();
    bad_kind.job_id = Some("job_bad_kind".to_string());
    bad_kind.kind = "compact_cards".to_string();
    let kind_err = svc
        .run_dream_job(bad_kind)
        .expect_err("non-preview kind should be rejected");
    assert!(kind_err.message.contains("kind must be dream_preview"));

    let mut bad_now = base_request();
    bad_now.job_id = Some("job_bad_now".to_string());
    bad_now.now = Some("not-a-time".to_string());
    let now_err = svc
        .run_dream_job(bad_now)
        .expect_err("invalid now must be rejected");
    assert!(now_err.message.contains("now must be an RFC3339"));

    let mut bad_since = base_request();
    bad_since.job_id = Some("job_bad_since".to_string());
    bad_since.since = Some("not-a-time".to_string());
    let since_err = svc
        .run_dream_job(bad_since)
        .expect_err("invalid since must be rejected");
    assert!(since_err.message.contains("since must be an RFC3339"));
}

#[test]
fn deterministic_job_run_preview_preserves_evidence_refs() {
    let svc = service();
    conclude(
        &svc,
        "I prefer concise commit messages and deterministic release scripts.",
    );
    let run = svc
        .run_dream_job(DreamJobRunRequest {
            job_id: Some("job_evidence_refs".to_string()),
            ..base_request()
        })
        .expect("job run should succeed");

    let has_candidate_refs = run
        .preview
        .candidates
        .iter()
        .any(|candidate| !candidate.evidence_refs.is_empty());
    assert!(
        has_candidate_refs,
        "preview candidates should preserve evidence refs"
    );
}

#[test]
fn local_model_job_is_typed_preview_only_and_audits_provenance() {
    let svc = service();
    conclude(&svc, "I prefer concise commit messages.");
    let before = svc.store.count_records().unwrap();
    let (endpoint, server) = fake_provider();
    let mut req = base_request();
    req.job_id = Some("job_local_model".to_string());
    req.mode = Some("local-model".to_string());
    req.provider = Some(DreamJobProvider {
        adapter: Some(DreamProviderAdapter::LocalModel),
        endpoint: Some(endpoint),
        model: Some("fake-local".to_string()),
        provider: Some("fake-local-runtime".to_string()),
        ..Default::default()
    });
    req.budget.max_input_bytes = 64 * 1024;
    req.budget.max_input_tokens = 16 * 1024;
    req.budget.max_output_bytes = 64 * 1024;
    req.budget.max_output_tokens = 16 * 1024;
    req.budget.max_provider_calls = 1;

    let run = svc.run_dream_job(req).expect("local model preview");
    server.join().expect("provider server");

    assert_eq!(run.status, "ok");
    assert_eq!(run.preview.mode, "preview");
    assert_eq!(run.preview.authority, "recall_not_authority");
    let candidate = run
        .preview
        .candidates
        .iter()
        .find(|candidate| candidate.provenance.is_some())
        .expect("model candidate");
    assert!(!candidate.evidence_refs.is_empty());
    assert!(!candidate.apply_eligible);
    assert_eq!(
        candidate.provenance.as_ref().unwrap().adapter,
        "local-model"
    );
    assert_eq!(svc.store.count_records().unwrap(), before);
    let job = svc.store.get_dream_job("job_local_model").unwrap().unwrap();
    assert_eq!(job.provider.model.as_deref(), Some("fake-local"));
    assert_eq!(job.provider.adapter, Some(DreamProviderAdapter::LocalModel));
}
