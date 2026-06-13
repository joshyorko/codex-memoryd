//! Fixture-driven Dreamer eval harness.
//!
//! The harness is intentionally deterministic and model-free: each JSONL
//! scenario is loaded as chronological evidence, previewed by stable heuristics,
//! and applied through the existing Store/recall paths.

use std::path::{Path, PathBuf};

use codex_memoryd::config::Config;
use codex_memoryd::domain::RecordType;
use codex_memoryd::domain::RepoIdentity;
use codex_memoryd::ids;
use codex_memoryd::policy;
use codex_memoryd::protocol::DreamRequest;
use codex_memoryd::protocol::RecallRequest;
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store};
use serde::Deserialize;
use serde_json::{json, Value};

const PROFILE: &str = "personal";
const WORKSPACE: &str = "dream-eval";

#[derive(Debug, Deserialize)]
struct FixtureEvent {
    kind: String,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    now: Option<String>,
    created_at: String,
}

impl FixtureEvent {
    fn text(&self) -> Option<&str> {
        self.content.as_deref().or(self.summary.as_deref())
    }
}

#[derive(Debug)]
struct FixtureLine {
    event: FixtureEvent,
}

#[derive(Debug)]
struct LoadedFixture {
    name: String,
    path: PathBuf,
    lines: Vec<FixtureLine>,
}

#[derive(Debug, Deserialize)]
struct Sidecar {
    expect_preview: PreviewExpectation,
    #[serde(default)]
    expect_apply: Option<ApplyExpectation>,
    #[serde(default)]
    expect_recall_before: Option<RecallExpectation>,
    #[serde(default)]
    expect_recall_after: Option<RecallExpectation>,
}

#[derive(Debug, Deserialize)]
struct PreviewExpectation {
    #[serde(default)]
    accepted: Vec<CandidateExpectation>,
    #[serde(default)]
    rejected: Vec<CandidateExpectation>,
    #[serde(default)]
    quarantined: Vec<CandidateExpectation>,
    #[serde(default)]
    stale: Vec<StaleExpectation>,
    #[serde(default)]
    rejected_items: Vec<RejectedExpectation>,
}

#[derive(Debug, Deserialize)]
struct CandidateExpectation {
    subject_key: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    record_type: Option<String>,
    #[serde(default)]
    promotion_reason: Option<String>,
    #[serde(default)]
    evidence_count: Option<usize>,
    #[serde(default)]
    must_contain: Vec<String>,
    #[serde(default)]
    must_not_contain: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct StaleExpectation {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    drift_prone: Option<bool>,
    #[serde(default)]
    suggested_action: Option<String>,
    #[serde(default)]
    historical_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RejectedExpectation {
    #[serde(default)]
    reason_contains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyExpectation {
    created: usize,
    archived: usize,
    idempotent_second_apply: bool,
}

#[derive(Debug, Deserialize)]
struct RecallExpectation {
    query: String,
    #[serde(default)]
    must_contain: Vec<String>,
    #[serde(default)]
    must_not_contain: Vec<String>,
}

struct SeededFixture {
    store: Store,
    service: Service,
    repo: Option<RepoIdentity>,
    now: String,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dreaming")
}

fn load_fixture(path: &Path) -> Result<LoadedFixture, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    parse_fixture_jsonl(path, &raw)
}

fn parse_fixture_jsonl(path: &Path, raw: &str) -> Result<LoadedFixture, String> {
    let mut lines = Vec::new();
    for (idx, raw_line) in raw.lines().enumerate() {
        let line_no = idx + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event: FixtureEvent = serde_json::from_str(trimmed)
            .map_err(|e| format!("{}:{line_no}: invalid JSONL event: {e}", path.display()))?;
        validate_event(path, line_no, &event)?;
        lines.push(FixtureLine { event });
    }
    if lines.is_empty() {
        return Err(format!(
            "{}: fixture must contain at least one event",
            path.display()
        ));
    }
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: fixture has no valid file stem", path.display()))?
        .to_string();
    Ok(LoadedFixture {
        name,
        path: path.to_path_buf(),
        lines,
    })
}

fn validate_event(path: &Path, line: usize, event: &FixtureEvent) -> Result<(), String> {
    if event.created_at.trim().is_empty() {
        return Err(format!("{}:{line}: created_at is required", path.display()));
    }
    match event.kind.as_str() {
        "visible_turn" => {
            let actor = event.actor.as_deref().ok_or_else(|| {
                format!("{}:{line}: visible_turn.actor is required", path.display())
            })?;
            if actor != "user" && actor != "assistant" {
                return Err(format!(
                    "{}:{line}: visible_turn.actor must be user or assistant",
                    path.display()
                ));
            }
            require_text(path, line, event, "visible_turn.content")?;
        }
        "conclusion" => {
            require_text(path, line, event, "conclusion.content")?;
        }
        "checkpoint" => {
            if event
                .summary
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(format!(
                    "{}:{line}: checkpoint.summary is required",
                    path.display()
                ));
            }
        }
        "memory_record" => {
            require_text(path, line, event, "memory_record.content")?;
            let raw_type = event.record_type.as_deref().ok_or_else(|| {
                format!("{}:{line}: memory_record.type is required", path.display())
            })?;
            if RecordType::parse(raw_type).is_none() {
                return Err(format!(
                    "{}:{line}: unknown memory_record.type `{raw_type}`",
                    path.display()
                ));
            }
        }
        "dream_clock" => {
            if event.now.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(format!(
                    "{}:{line}: dream_clock.now is required",
                    path.display()
                ));
            }
        }
        other => {
            return Err(format!(
                "{}:{line}: unknown event kind `{other}`",
                path.display()
            ));
        }
    }
    Ok(())
}

fn require_text(path: &Path, line: usize, event: &FixtureEvent, field: &str) -> Result<(), String> {
    if event.text().unwrap_or_default().trim().is_empty() {
        return Err(format!("{}:{line}: {field} is required", path.display()));
    }
    Ok(())
}

fn load_sidecar(fixture: &LoadedFixture) -> Sidecar {
    let path = fixture.path.with_extension("expected.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read sidecar {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse sidecar {}: {e}", path.display()))
}

fn seed_real_dream_store(fixture: &LoadedFixture) -> SeededFixture {
    let store = Store::open(":memory:").expect("open in-memory store");
    store
        .ensure_workspace(PROFILE, WORKSPACE)
        .expect("workspace");
    let config = Config {
        default_workspace: WORKSPACE.to_string(),
        ..Default::default()
    };
    let service = Service::new(store.clone(), config);

    let mut repo_id = None;
    let mut dream_now = None;
    let mut session_ids = std::collections::BTreeMap::<Option<String>, String>::new();

    for line in &fixture.lines {
        let event = &line.event;
        if event.repo_id.is_some() {
            repo_id = event.repo_id.clone();
        }
        match event.kind.as_str() {
            "dream_clock" => {
                dream_now = event.now.clone().or_else(|| Some(event.created_at.clone()));
            }
            "visible_turn" => {
                let repo_id = event.repo_id.as_deref().or(repo_id.as_deref());
                seed_visible_turn(&store, event, repo_id, &mut session_ids);
            }
            "conclusion" => {
                let repo_id = event.repo_id.as_deref().or(repo_id.as_deref());
                seed_conclusion(&store, event, repo_id);
            }
            "checkpoint" => {
                let repo_id = event.repo_id.as_deref().or(repo_id.as_deref());
                seed_checkpoint(&store, event, repo_id, &mut session_ids);
            }
            "memory_record" => {
                let repo_id = event.repo_id.as_deref().or(repo_id.as_deref());
                seed_memory_record_event(&store, event, repo_id);
            }
            other => panic!("missing real Dreamer seed for fixture event kind {other}"),
        }
    }

    let repo = repo_id.map(|repo_id| RepoIdentity {
        repo_id,
        ..Default::default()
    });
    let now = dream_now.unwrap_or_else(|| {
        fixture
            .lines
            .iter()
            .map(|line| line.event.created_at.clone())
            .max()
            .unwrap_or_else(|| "2026-06-13T00:00:00Z".to_string())
    });

    SeededFixture {
        store,
        service,
        repo,
        now,
    }
}

fn seed_visible_turn(
    store: &Store,
    event: &FixtureEvent,
    repo_id: Option<&str>,
    session_ids: &mut std::collections::BTreeMap<Option<String>, String>,
) {
    let session_key = repo_id.map(str::to_string);
    let session_id = session_ids
        .entry(session_key.clone())
        .or_insert_with(|| ids::new_id("sess"))
        .clone();
    store
        .ensure_session(
            &session_id,
            PROFILE,
            WORKSPACE,
            repo_id,
            None,
            "dream-fixture",
        )
        .expect("visible_turn session");
    let content = event.text().expect("visible_turn content");
    let actor = event.actor.as_deref().expect("visible_turn actor");
    store
        .insert_visible_turn(&codex_memoryd::domain::VisibleTurn {
            id: ids::new_id("turn"),
            session_id: session_id.clone(),
            actor: actor.to_string(),
            content: content.to_string(),
            created_at: event.created_at.clone(),
            metadata: json!({
                "origin": "visible_turn",
                "actor": actor,
                "created_at": event.created_at.clone(),
            }),
        })
        .expect("visible_turn");
    seed_corresponding_memory_record(
        store,
        repo_id,
        content,
        json!({
            "origin": "visible_turn",
            "actor": actor,
            "created_at": event.created_at.clone(),
        }),
        None,
        vec![],
    );
}

fn seed_conclusion(store: &Store, event: &FixtureEvent, repo_id: Option<&str>) {
    let content = event.text().expect("conclusion content");
    store
        .insert_conclusion(&codex_memoryd::domain::Conclusion {
            id: ids::new_id("concl"),
            profile_id: PROFILE.to_string(),
            workspace_id: WORKSPACE.to_string(),
            repo_id: repo_id.map(str::to_string),
            target: "user".to_string(),
            content: content.to_string(),
            source_id: None,
            created_at: event.created_at.clone(),
            metadata: json!({
                "origin": "conclusion",
                "target": "user",
                "created_at": event.created_at.clone(),
            }),
        })
        .expect("conclusion");
    seed_corresponding_memory_record(
        store,
        repo_id,
        content,
        json!({
            "origin": "conclusion",
            "target": "user",
            "created_at": event.created_at.clone(),
        }),
        None,
        vec![],
    );
}

fn seed_checkpoint(
    store: &Store,
    event: &FixtureEvent,
    repo_id: Option<&str>,
    session_ids: &mut std::collections::BTreeMap<Option<String>, String>,
) {
    let session_key = repo_id.map(str::to_string);
    let session_id = session_ids
        .entry(session_key.clone())
        .or_insert_with(|| ids::new_id("sess"))
        .clone();
    store
        .ensure_session(
            &session_id,
            PROFILE,
            WORKSPACE,
            repo_id,
            None,
            "dream-fixture",
        )
        .expect("checkpoint session");
    let summary = event.summary.as_deref().expect("checkpoint summary");
    store
        .insert_checkpoint(&codex_memoryd::domain::Checkpoint {
            id: ids::new_id("ckpt"),
            session_id: Some(session_id.clone()),
            profile_id: PROFILE.to_string(),
            workspace_id: WORKSPACE.to_string(),
            repo_id: repo_id.map(str::to_string),
            summary: summary.to_string(),
            changed_files: vec![],
            decisions: vec![],
            blockers: vec![],
            next_steps: vec![],
            tests_run: vec![],
            tests_not_run: vec![],
            branch: None,
            commit: None,
            created_at: event.created_at.clone(),
        })
        .expect("checkpoint");
    seed_corresponding_memory_record(
        store,
        repo_id,
        summary,
        json!({
            "origin": "checkpoint",
            "created_at": event.created_at.clone(),
        }),
        Some(RecordType::TaskCheckpoint),
        vec![],
    );
}

fn seed_memory_record_event(store: &Store, event: &FixtureEvent, repo_id: Option<&str>) {
    let content = event.text().expect("memory_record content");
    let mut metadata = json!({
        "origin": "memory_record",
        "created_at": event.created_at.clone(),
    });
    if let Some(id) = event.id.as_deref() {
        metadata["id"] = Value::String(id.to_string());
        if id.starts_with("imported_") {
            metadata["origin"] = Value::String("codex-local-memory".to_string());
            metadata["artifact_kind"] = Value::String("memory_summary".to_string());
        }
    }
    if let Some(record_type) = event.record_type.as_deref() {
        metadata["type"] = Value::String(record_type.to_string());
    }
    let source_ids = event.id.iter().cloned().collect::<Vec<_>>();
    seed_corresponding_memory_record(store, repo_id, content, metadata, None, source_ids);
}

fn seed_corresponding_memory_record(
    store: &Store,
    repo_id: Option<&str>,
    content: &str,
    metadata: Value,
    record_type_override: Option<RecordType>,
    source_ids: Vec<String>,
) {
    let class = policy::classify(
        content,
        codex_memoryd::domain::Profile::Personal,
        repo_id.is_some(),
    );
    let record_type = record_type_override.unwrap_or(class.record_type);
    let content_hash = ids::content_hash(
        PROFILE,
        WORKSPACE,
        repo_id,
        record_type.as_str(),
        class.scope.as_str(),
        content,
    );
    let new = NewRecord {
        profile_id: PROFILE.to_string(),
        workspace_id: WORKSPACE.to_string(),
        repo_id: repo_id.map(str::to_string),
        scope: class.scope,
        record_type,
        content: content.to_string(),
        related_files: class.related_files,
        tags: class.tags,
        sensitivity: class.sensitivity,
        portability: class.portability,
        confidence: class.confidence,
        source_ids,
        content_hash,
        supersedes: vec![],
        metadata,
    };
    store.upsert_record(&new).expect("seed real dream record");
}

fn recall_text(service: &Service, expectation: &RecallExpectation) -> String {
    let resp = service
        .recall(RecallRequest {
            profile: Some(PROFILE.to_string()),
            workspace: Some(WORKSPACE.to_string()),
            repo: None,
            session: None,
            query: Some(expectation.query.clone()),
            files: vec![],
            max_tokens: Some(1000),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            metadata: None,
        })
        .expect("recall runs");
    resp.facts
        .iter()
        .map(|fact| fact.content.as_str())
        .chain(
            resp.checkpoints
                .iter()
                .map(|checkpoint| checkpoint.summary.as_str()),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_recall(label: &str, text: &str, expected: &RecallExpectation) {
    for needle in &expected.must_contain {
        assert!(
            text.contains(needle),
            "{label} recall for query `{}` must contain `{needle}`; got `{text}`",
            expected.query
        );
    }
    for needle in &expected.must_not_contain {
        assert!(
            !text.contains(needle),
            "{label} recall for query `{}` must not contain `{needle}`; got `{text}`",
            expected.query
        );
    }
}

fn assert_service_preview_matches(
    fixture: &LoadedFixture,
    response: &codex_memoryd::protocol::DreamResponse,
    expected: &PreviewExpectation,
) {
    let accepted = response
        .candidates
        .iter()
        .filter(|cand| cand.candidate_state == "accepted")
        .cloned()
        .collect::<Vec<_>>();
    let quarantined = response
        .candidates
        .iter()
        .filter(|cand| cand.candidate_state == "quarantined")
        .cloned()
        .collect::<Vec<_>>();

    assert_service_candidates(
        &fixture.name,
        "accepted",
        &accepted,
        &expected.accepted,
        true,
    );
    assert_service_candidates(
        &fixture.name,
        "quarantined",
        &quarantined,
        &expected.quarantined,
        true,
    );
    assert_service_stale(&fixture.name, &response.stale, &expected.stale);
    assert_service_rejections(
        &fixture.name,
        &response.rejected,
        &expected.rejected,
        &expected.rejected_items,
    );
}

fn assert_service_candidates(
    fixture_name: &str,
    bucket: &str,
    actual: &[codex_memoryd::protocol::DreamCandidate],
    expected: &[CandidateExpectation],
    require_no_leftovers: bool,
) {
    let mut used = vec![false; actual.len()];
    for exp in expected {
        let mut idx = None;
        for (candidate_idx, cand) in actual.iter().enumerate() {
            if used[candidate_idx] {
                continue;
            }
            if !candidate_matches_expectation(bucket, cand, exp) {
                continue;
            }
            idx = Some(candidate_idx);
            break;
        }
        let matched_idx = idx.unwrap_or_else(|| {
            panic!(
                "{fixture_name} missing {bucket} candidate {}",
                exp.subject_key
            )
        });
        used[matched_idx] = true;
        let cand = &actual[matched_idx];
        assert_eq!(
            cand.candidate_state, bucket,
            "{fixture_name} {} bucket state",
            exp.subject_key
        );
        if let Some(record_type) = &exp.record_type {
            assert_eq!(
                cand.proposed_type.as_str(),
                record_type.as_str(),
                "{} record_type",
                exp.subject_key
            );
        }
        if let Some(reason) = &exp.promotion_reason {
            assert_eq!(
                &cand.promotion_reason, reason,
                "{} promotion_reason",
                exp.subject_key
            );
        }
        if let Some(count) = exp.evidence_count {
            assert_eq!(
                cand.evidence_count, count,
                "{} evidence_count",
                exp.subject_key
            );
        }
        for needle in &exp.must_contain {
            assert!(
                cand.content.contains(needle),
                "{fixture_name} {bucket} candidate {} content must contain `{needle}`; got `{}`",
                exp.subject_key,
                cand.content
            );
        }
        for needle in &exp.must_not_contain {
            assert!(
                !cand.content.contains(needle),
                "{fixture_name} {bucket} candidate {} content must not contain `{needle}`; got `{}`",
                exp.subject_key,
                cand.content
            );
        }
        assert_eq!(
            cand.subject_key, exp.subject_key,
            "{fixture_name} {} candidate {} subject key",
            exp.subject_key, exp.subject_key
        );
    }

    if require_no_leftovers {
        let leftovers: Vec<&codex_memoryd::protocol::DreamCandidate> = actual
            .iter()
            .zip(used.iter())
            .filter_map(|(cand, was_used)| if !was_used { Some(cand) } else { None })
            .collect();
        assert!(
            leftovers.is_empty(),
            "{fixture_name} unexpected {bucket} candidate count ({}): {:?}",
            leftovers.len(),
            leftovers
                .iter()
                .map(|cand| &cand.subject_key)
                .collect::<Vec<_>>()
        );
    }
}

fn candidate_matches_expectation(
    bucket: &str,
    cand: &codex_memoryd::protocol::DreamCandidate,
    exp: &CandidateExpectation,
) -> bool {
    if cand.candidate_state != bucket {
        return false;
    }
    if let Some(action) = &exp.action {
        if cand.action != *action {
            return false;
        }
    }
    if let Some(record_type) = &exp.record_type {
        if cand.proposed_type != *record_type {
            return false;
        }
    }
    if let Some(reason) = &exp.promotion_reason {
        if cand.promotion_reason != *reason {
            return false;
        }
    }
    if let Some(count) = exp.evidence_count {
        if cand.evidence_count != count {
            return false;
        }
    }
    for needle in &exp.must_contain {
        if !cand.content.contains(needle) {
            return false;
        }
    }
    for needle in &exp.must_not_contain {
        if cand.content.contains(needle) {
            return false;
        }
    }
    cand.subject_key == exp.subject_key
}

fn assert_service_rejections(
    fixture_name: &str,
    actual: &[codex_memoryd::protocol::DreamRejection],
    expected: &[CandidateExpectation],
    expected_items: &[RejectedExpectation],
) {
    if expected.is_empty() {
        assert_service_rejection_reasons(fixture_name, actual, expected_items);
        return;
    }

    let mut used = vec![false; actual.len()];
    for exp in expected {
        let mut idx = None;
        for (actual_idx, rejection) in actual.iter().enumerate() {
            if used[actual_idx] {
                continue;
            }
            if !rejection_matches_expectation(rejection, exp) {
                continue;
            }
            idx = Some(actual_idx);
            break;
        }
        let matched_idx = idx.unwrap_or_else(|| {
            panic!(
                "{fixture_name} missing rejected candidate {}",
                exp.subject_key
            )
        });
        used[matched_idx] = true;
    }

    let leftovers: Vec<&codex_memoryd::protocol::DreamRejection> = actual
        .iter()
        .zip(used.iter())
        .filter_map(|(rej, was_used)| if !was_used { Some(rej) } else { None })
        .collect();
    assert!(
        leftovers.is_empty(),
        "{fixture_name} unexpected rejected count ({}): {:?}",
        leftovers.len(),
        leftovers.iter().map(|rej| &rej.reason).collect::<Vec<_>>()
    );

    if !expected_items.is_empty() {
        assert_service_rejection_reasons(fixture_name, actual, expected_items);
    }
}

fn rejection_matches_expectation(
    actual: &codex_memoryd::protocol::DreamRejection,
    exp: &CandidateExpectation,
) -> bool {
    for needle in &exp.must_contain {
        if !actual.reason.contains(needle) {
            return false;
        }
    }
    for needle in &exp.must_not_contain {
        if actual.reason.contains(needle) {
            return false;
        }
    }
    true
}

fn assert_service_stale(
    fixture_name: &str,
    actual: &[codex_memoryd::protocol::DreamStaleRecord],
    expected: &[StaleExpectation],
) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{fixture_name} stale candidate count"
    );
    for (stale, exp) in actual.iter().zip(expected) {
        if let Some(state) = &exp.state {
            assert_eq!(&stale.state, state, "{fixture_name} stale state");
        }
        if let Some(drift_prone) = exp.drift_prone {
            assert_eq!(stale.drift_prone, drift_prone, "{fixture_name} drift_prone");
        }
        if let Some(action) = &exp.suggested_action {
            assert_eq!(
                &stale.suggested_action, action,
                "{fixture_name} suggested_action"
            );
        }
        if let Some(reason) = &exp.historical_reason {
            assert_eq!(
                stale.historical_reason.as_deref(),
                Some(reason.as_str()),
                "{fixture_name} historical_reason"
            );
        }
    }
}

fn assert_service_rejection_reasons(
    fixture_name: &str,
    actual: &[codex_memoryd::protocol::DreamRejection],
    expected: &[RejectedExpectation],
) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{fixture_name} rejected candidate count"
    );
    for (rejection, exp) in actual.iter().zip(expected) {
        for needle in &exp.reason_contains {
            assert!(
                rejection.reason.contains(needle),
                "{fixture_name} rejected reason must contain `{needle}`; got `{}`",
                rejection.reason
            );
        }
    }
}

#[test]
fn dreaming_jsonl_fixture_loader_parses_existing_fixtures() {
    let fixtures = [
        "repeated_preference.jsonl",
        "stale_time_sensitive_fact.jsonl",
        "conflicting_newer_fact.jsonl",
        "secret_rejection.jsonl",
        "repo_gotcha.jsonl",
        "user_adopts_assistant_proposal.jsonl",
        "assistant_proposal_without_adoption.jsonl",
        "single_mention_preference_not_promoted.jsonl",
        "imported_memory_self_reinforcement_blocked.jsonl",
        "explicit_conclusion_promotes.jsonl",
        "repeated_user_steering_promotes.jsonl",
    ];
    for fixture in fixtures {
        let loaded = load_fixture(&fixtures_dir().join(fixture)).expect("fixture parses");
        assert!(!loaded.lines.is_empty(), "{fixture} has events");
    }
}

#[test]
fn dreaming_jsonl_fixture_loader_reports_useful_errors() {
    let path = fixtures_dir().join("bad_fixture.jsonl");
    let err = parse_fixture_jsonl(&path, "{\"kind\":\"visible_turn\",\"content\":\"missing actor\",\"created_at\":\"2026-01-01T00:00:00Z\"}\n")
        .expect_err("bad event must fail");
    assert!(
        err.contains("bad_fixture.jsonl:1"),
        "error includes file and line: {err}"
    );
    assert!(
        err.contains("visible_turn.actor is required"),
        "error explains missing field: {err}"
    );
}

#[test]
fn dreaming_sidecars_are_stable_and_readable() {
    for entry in std::fs::read_dir(fixtures_dir()).expect("read dreaming fixtures") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let fixture = load_fixture(&path).expect("fixture parses");
        let sidecar = load_sidecar(&fixture);
        let assertion_count = sidecar.expect_preview.accepted.len()
            + sidecar.expect_preview.rejected.len()
            + sidecar.expect_preview.quarantined.len()
            + sidecar.expect_preview.stale.len()
            + sidecar.expect_preview.rejected_items.len();
        assert!(
            assertion_count > 0,
            "{} has explicit preview assertions",
            fixture.name
        );
        for bucket in [
            &sidecar.expect_preview.accepted,
            &sidecar.expect_preview.rejected,
            &sidecar.expect_preview.quarantined,
        ] {
            for candidate in bucket.iter() {
                assert!(
                    !candidate.subject_key.trim().is_empty(),
                    "{} sidecar candidate has readable subject_key",
                    fixture.name
                );
            }
        }
        for stale in &sidecar.expect_preview.stale {
            assert!(
                stale.state.is_some()
                    || stale.drift_prone.is_some()
                    || stale.suggested_action.is_some()
                    || stale.historical_reason.is_some(),
                "{} stale sidecar should describe the real service fields",
                fixture.name
            );
        }
    }
}

#[test]
fn dreaming_fixtures_preview_apply_and_recall_from_sidecars() {
    for entry in std::fs::read_dir(fixtures_dir()).expect("read dreaming fixtures") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let fixture = load_fixture(&path).expect("fixture parses");
        let expected = load_sidecar(&fixture);
        let seeded = seed_real_dream_store(&fixture);
        let now = seeded.now.clone();
        let repo = seeded.repo.clone();
        let preview = seeded
            .service
            .dream(DreamRequest {
                profile: Some(PROFILE.to_string()),
                workspace: Some(WORKSPACE.to_string()),
                repo: repo.clone(),
                mode: Some("preview".to_string()),
                now: Some(now.clone()),
                since: None,
            })
            .expect("dream preview runs");

        if matches!(
            fixture.name.as_str(),
            "conflicting_newer_fact" | "repeated_preference" | "repo_gotcha"
        ) {
            eprintln!("{preview:#?}");
        }

        assert_service_preview_matches(&fixture, &preview, &expected.expect_preview);

        if let Some(recall_before) = &expected.expect_recall_before {
            let text = recall_text(&seeded.service, recall_before);
            assert_recall(&format!("{} before", fixture.name), &text, recall_before);
        }

        if let Some(apply_expected) = &expected.expect_apply {
            let first = seeded
                .service
                .dream(DreamRequest {
                    profile: Some(PROFILE.to_string()),
                    workspace: Some(WORKSPACE.to_string()),
                    repo: repo.clone(),
                    mode: Some("apply".to_string()),
                    now: Some(now.clone()),
                    since: None,
                })
                .expect("dream apply runs");
            assert_eq!(
                first.created.len(),
                apply_expected.created,
                "{} first apply created",
                fixture.name
            );
            assert_eq!(
                first.archived.len(),
                apply_expected.archived,
                "{} first apply archived",
                fixture.name
            );

            if let Some(recall_after) = &expected.expect_recall_after {
                let text = recall_text(&seeded.service, recall_after);
                assert_recall(&format!("{} after", fixture.name), &text, recall_after);
            }

            if apply_expected.idempotent_second_apply {
                let active_after_first = seeded.store.count_records().expect("count records");
                let second = seeded
                    .service
                    .dream(DreamRequest {
                        profile: Some(PROFILE.to_string()),
                        workspace: Some(WORKSPACE.to_string()),
                        repo: repo.clone(),
                        mode: Some("apply".to_string()),
                        now: Some(now.clone()),
                        since: None,
                    })
                    .expect("dream second apply runs");
                assert!(
                    second.created.is_empty() && second.archived.is_empty(),
                    "{} second apply creates no duplicate active records and archives nothing new",
                    fixture.name
                );
                assert_eq!(
                    seeded.store.count_records().expect("count records"),
                    active_after_first,
                    "{} second apply keeps record count stable",
                    fixture.name
                );
            }
        }
    }
}
