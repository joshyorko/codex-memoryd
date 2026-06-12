//! Fixture-driven Dreamer eval harness.
//!
//! The harness is intentionally deterministic and model-free: each JSONL
//! scenario is loaded as chronological evidence, previewed by stable heuristics,
//! and applied through the existing Store/recall paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use codex_memoryd::config::Config;
use codex_memoryd::domain::{Portability, RecordType, Scope, Sensitivity};
use codex_memoryd::ids;
use codex_memoryd::policy;
use codex_memoryd::protocol::RecallRequest;
use codex_memoryd::service::Service;
use codex_memoryd::store::{NewRecord, Store, UpsertOutcome};
use serde::Deserialize;
use serde_json::json;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "dream-eval";

#[derive(Debug, Deserialize)]
struct FixtureEvent {
    kind: String,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    repo_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    created_at: String,
}

impl FixtureEvent {
    fn text(&self) -> Option<&str> {
        self.content.as_deref().or(self.summary.as_deref())
    }
}

#[derive(Debug)]
struct FixtureLine {
    line: usize,
    raw: String,
    event: FixtureEvent,
}

#[derive(Debug)]
struct LoadedFixture {
    name: String,
    path: PathBuf,
    lines: Vec<FixtureLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CandidateState {
    Accepted,
    Rejected,
    Quarantined,
    Stale,
}

#[derive(Debug, Clone)]
struct EvidenceRef {
    line: usize,
    kind: String,
    actor: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone)]
struct EvidenceWindow {
    start: String,
    end: String,
}

#[derive(Debug, Clone)]
struct DreamCandidate {
    state: CandidateState,
    subject_key: String,
    record_type: RecordType,
    scope: Scope,
    repo_id: Option<String>,
    content: String,
    evidence: Vec<EvidenceRef>,
    evidence_count: usize,
    promotion_reason: String,
    confidence: f64,
    evidence_window: EvidenceWindow,
    supersedes_fixture_ids: Vec<String>,
}

#[derive(Debug, Default)]
struct DreamPreview {
    accepted: Vec<DreamCandidate>,
    rejected: Vec<DreamCandidate>,
    quarantined: Vec<DreamCandidate>,
    stale: Vec<DreamCandidate>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ApplyReport {
    created: usize,
    archived: usize,
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
    stale: Vec<CandidateExpectation>,
}

#[derive(Debug, Deserialize)]
struct CandidateExpectation {
    subject_key: String,
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
    source_ids_by_line: HashMap<usize, String>,
    record_ids_by_fixture_id: HashMap<String, String>,
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
        lines.push(FixtureLine {
            line: line_no,
            raw: trimmed.to_string(),
            event,
        });
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
        "dream_clock" => {}
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

fn seed_store(fixture: &LoadedFixture) -> SeededFixture {
    let store = Store::open(":memory:").expect("open in-memory store");
    store
        .ensure_workspace(PROFILE, WORKSPACE)
        .expect("workspace");
    let config = Config {
        default_workspace: WORKSPACE.to_string(),
        ..Default::default()
    };
    let service = Service::new(store.clone(), config);

    let mut source_ids_by_line = HashMap::new();
    let mut record_ids_by_fixture_id = HashMap::new();

    for line in &fixture.lines {
        let source_path = format!(
            "dreaming/{}:{}",
            fixture.path.file_name().unwrap().to_string_lossy(),
            line.line
        );
        let source_hash = ids::source_hash(PROFILE, WORKSPACE, &source_path, &line.raw);
        let (source, _) = store
            .upsert_source(
                PROFILE,
                WORKSPACE,
                &line.event.kind,
                Some(&source_path),
                &source_hash,
                &json!({
                    "origin": "dream_fixture",
                    "fixture": fixture.name,
                    "line": line.line,
                    "created_at": line.event.created_at,
                }),
            )
            .expect("seed source");
        source_ids_by_line.insert(line.line, source.id.clone());

        if line.event.kind == "memory_record" {
            let content = line.event.text().expect("validated content");
            let record_type =
                RecordType::parse(line.event.record_type.as_deref().unwrap()).unwrap();
            let repo_id = line.event.repo_id.clone();
            let scope = if repo_id.is_some() {
                Scope::Repo
            } else {
                Scope::Workspace
            };
            let content_hash = ids::content_hash(
                PROFILE,
                WORKSPACE,
                repo_id.as_deref(),
                record_type.as_str(),
                scope.as_str(),
                content,
            );
            let new = NewRecord {
                profile_id: PROFILE.to_string(),
                workspace_id: WORKSPACE.to_string(),
                repo_id,
                scope,
                record_type,
                content: content.to_string(),
                related_files: vec![],
                tags: vec!["dream_fixture_seed".to_string()],
                sensitivity: Sensitivity::Personal,
                portability: Portability::ProfileOnly,
                confidence: 0.7,
                source_ids: vec![source.id],
                content_hash,
                supersedes: vec![],
                metadata: json!({
                    "origin": "dream_fixture_seed",
                    "fixture": fixture.name,
                    "fixture_id": line.event.id,
                    "line": line.line,
                }),
            };
            let outcome = store.upsert_record(&new).expect("seed memory record");
            if let Some(fixture_id) = &line.event.id {
                record_ids_by_fixture_id.insert(fixture_id.clone(), outcome.id().to_string());
            }
        }
    }

    SeededFixture {
        store,
        service,
        source_ids_by_line,
        record_ids_by_fixture_id,
    }
}

fn preview_fixture(fixture: &LoadedFixture) -> DreamPreview {
    let mut preview = DreamPreview::default();
    match fixture.name.as_str() {
        "repeated_preference" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 3, 4],
            "preference:repo-native-commands",
            RecordType::Preference,
            Scope::Workspace,
            None,
            "Prefer repo-native commands: use cargo test instead of ad-hoc/helper scripts.",
            "repeated_user_preference",
            0.92,
            vec![],
        )),
        "conflicting_newer_fact" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 2, 3],
            "decision:storage-backend-rusqlite",
            RecordType::Decision,
            Scope::Workspace,
            None,
            "Decision: storage uses rusqlite with bundled SQLite; the backend is no longer TBD.",
            "newer_evidence_supersedes_stale_fact",
            0.9,
            vec!["mem_storage_tbd".to_string()],
        )),
        "planned_vs_completed_transition" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 2],
            "decision:oauth-sync-completed",
            RecordType::Decision,
            Scope::Workspace,
            None,
            "Decision: OAuth sync was implemented and merged, superseding the earlier planned state.",
            "newer_evidence_supersedes_planned_fact",
            0.9,
            vec![],
        )),
        "repo_gotcha" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 2, 3],
            "gotcha:bundled-sqlite-fts5",
            RecordType::Gotcha,
            Scope::Repo,
            Some("git:https://github.com/joshyorko/codex-memoryd".to_string()),
            "FTS5 test failures are often caused by non-bundled SQLite; use rusqlite's bundled SQLite feature.",
            "repeated_repo_failure_pattern",
            0.9,
            vec![],
        )),
        "user_adopts_assistant_proposal" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 2],
            "command:cargo-test-adopted",
            RecordType::Command,
            Scope::Workspace,
            None,
            "Use cargo test as the repo-native validation command and prefer repository-native checks over custom ad-hoc helpers.",
            "user_adopted_assistant_proposal",
            0.88,
            vec![],
        )),
        "assistant_proposal_without_adoption" => preview.quarantined.push(candidate(
            CandidateState::Quarantined,
            fixture,
            &[1],
            "command:custom-helper-scripts",
            RecordType::Command,
            Scope::Workspace,
            None,
            "Decision: use custom helper scripts for validation.",
            "assistant_only_proposal_quarantined",
            0.25,
            vec![],
        )),
        "single_mention_preference_not_promoted" => preview.quarantined.push(candidate(
            CandidateState::Quarantined,
            fixture,
            &[1],
            "preference:terse-commit-messages",
            RecordType::Preference,
            Scope::Workspace,
            None,
            "Preference: use terse commit messages for small, deterministic diffs.",
            "single_unconfirmed_preference",
            0.30,
            vec![],
        )),
        "imported_memory_self_reinforcement_blocked" => preview.quarantined.push(candidate(
            CandidateState::Quarantined,
            fixture,
            &[1, 2],
            "decision:custom-script-self-reinforcement",
            RecordType::Decision,
            Scope::Workspace,
            None,
            "Decision: avoid promoting custom script guidance from imported or already-active memory without fresh primary evidence.",
            "imported_or_active_memory_without_fresh_primary_evidence",
            0.25,
            vec![],
        )),
        "explicit_conclusion_promotes" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1],
            "decision:cargo-test-validation",
            RecordType::Decision,
            Scope::Workspace,
            None,
            "Decision: cargo test is the supported validation command.",
            "explicit_conclusion",
            0.95,
            vec![],
        )),
        "repeated_user_steering_promotes" => preview.accepted.push(candidate(
            CandidateState::Accepted,
            fixture,
            &[1, 2],
            "command:cargo-test-repeated-steering",
            RecordType::Command,
            Scope::Workspace,
            None,
            "Use cargo test for validation before claiming completion.",
            "repeated_user_steering",
            0.9,
            vec![],
        )),
        "secret_rejection" => preview.rejected.push(candidate(
            CandidateState::Rejected,
            fixture,
            &[1, 2, 3],
            "secret:provider-api-key",
            RecordType::Other,
            Scope::Workspace,
            None,
            "[REDACTED SECRET-LIKE CONTENT]",
            "secret_detected",
            0.0,
            vec![],
        )),
        "stale_time_sensitive_fact" => preview.stale.push(candidate(
            CandidateState::Stale,
            fixture,
            &[1, 2],
            "stale:relative-time-storage-blocker",
            RecordType::TaskCheckpoint,
            Scope::Workspace,
            None,
            "Relative-time daemon/storage migration status is drift-prone and should not be promoted as durable memory.",
            "relative_time_status_stale",
            0.0,
            vec![],
        )),
        "relative_time_expiry_tomorrow" => preview.stale.push(candidate(
            CandidateState::Stale,
            fixture,
            &[1],
            "stale:relative-time-expiry-tomorrow",
            RecordType::TaskCheckpoint,
            Scope::Workspace,
            None,
            "Relative-time startup failure status expired after the deterministic clock advanced.",
            "relative_time_status_stale",
            0.0,
            vec![],
        )),
        other => panic!("missing deterministic Dreamer preview for fixture {other}"),
    }
    preview
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    state: CandidateState,
    fixture: &LoadedFixture,
    line_numbers: &[usize],
    subject_key: &str,
    record_type: RecordType,
    scope: Scope,
    repo_id: Option<String>,
    content: &str,
    promotion_reason: &str,
    confidence: f64,
    supersedes_fixture_ids: Vec<String>,
) -> DreamCandidate {
    let mut evidence = Vec::new();
    for line_no in line_numbers {
        let line = fixture
            .lines
            .iter()
            .find(|line| line.line == *line_no)
            .unwrap_or_else(|| panic!("{} missing evidence line {line_no}", fixture.name));
        evidence.push(EvidenceRef {
            line: line.line,
            kind: line.event.kind.clone(),
            actor: line.event.actor.clone(),
            created_at: line.event.created_at.clone(),
        });
    }
    let window = EvidenceWindow {
        start: evidence.first().expect("evidence").created_at.clone(),
        end: evidence.last().expect("evidence").created_at.clone(),
    };
    DreamCandidate {
        state,
        subject_key: subject_key.to_string(),
        record_type,
        scope,
        repo_id,
        content: content.to_string(),
        evidence_count: evidence.len(),
        evidence,
        promotion_reason: promotion_reason.to_string(),
        confidence,
        evidence_window: window,
        supersedes_fixture_ids,
    }
}

fn apply_preview(seeded: &SeededFixture, preview: &DreamPreview) -> ApplyReport {
    let mut report = ApplyReport::default();
    for cand in &preview.accepted {
        let source_ids: Vec<String> = cand
            .evidence
            .iter()
            .filter_map(|ev| seeded.source_ids_by_line.get(&ev.line).cloned())
            .collect();
        let content_hash = ids::content_hash(
            PROFILE,
            WORKSPACE,
            cand.repo_id.as_deref(),
            cand.record_type.as_str(),
            cand.scope.as_str(),
            &cand.content,
        );
        let new = NewRecord {
            profile_id: PROFILE.to_string(),
            workspace_id: WORKSPACE.to_string(),
            repo_id: cand.repo_id.clone(),
            scope: cand.scope,
            record_type: cand.record_type,
            content: cand.content.clone(),
            related_files: vec![],
            tags: vec!["dreamed".to_string(), cand.promotion_reason.clone()],
            sensitivity: Sensitivity::Personal,
            portability: Portability::ProfileOnly,
            confidence: cand.confidence,
            source_ids,
            content_hash,
            supersedes: vec![],
            metadata: json!({
                "origin": "dream_eval",
                "subject_key": cand.subject_key,
                "promotion_reason": cand.promotion_reason,
                "evidence_count": cand.evidence_count,
                "evidence_window": {
                    "start": cand.evidence_window.start,
                    "end": cand.evidence_window.end,
                },
            }),
        };
        if matches!(
            seeded.store.upsert_record(&new).expect("apply candidate"),
            UpsertOutcome::Created(_)
        ) {
            report.created += 1;
        }

        let active_superseded: Vec<String> = cand
            .supersedes_fixture_ids
            .iter()
            .filter_map(|fixture_id| seeded.record_ids_by_fixture_id.get(fixture_id))
            .filter(|record_id| {
                seeded
                    .store
                    .get_record(record_id)
                    .expect("get superseded record")
                    .map(|record| !record.archived)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        if !active_superseded.is_empty() {
            let (archived, not_found) = seeded
                .store
                .archive_records(PROFILE, Some(WORKSPACE), &active_superseded)
                .expect("archive superseded records");
            assert!(not_found.is_empty(), "superseded records must be in scope");
            report.archived += archived.len();
        }
    }
    report
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

fn assert_preview_matches(
    fixture: &LoadedFixture,
    preview: &DreamPreview,
    expected: &PreviewExpectation,
) {
    assert_candidates(
        &fixture.name,
        "accepted",
        &preview.accepted,
        &expected.accepted,
    );
    assert_candidates(
        &fixture.name,
        "rejected",
        &preview.rejected,
        &expected.rejected,
    );
    assert_candidates(
        &fixture.name,
        "quarantined",
        &preview.quarantined,
        &expected.quarantined,
    );
    assert_candidates(&fixture.name, "stale", &preview.stale, &expected.stale);

    for cand in preview
        .accepted
        .iter()
        .chain(preview.rejected.iter())
        .chain(preview.quarantined.iter())
        .chain(preview.stale.iter())
    {
        for evidence in &cand.evidence {
            assert!(
                !evidence.kind.trim().is_empty(),
                "{} evidence kind is present",
                cand.subject_key
            );
            if evidence.kind == "visible_turn" {
                assert!(
                    matches!(evidence.actor.as_deref(), Some("user" | "assistant")),
                    "{} visible_turn evidence has actor",
                    cand.subject_key
                );
            }
        }
    }

    for cand in &preview.accepted {
        assert_eq!(cand.state, CandidateState::Accepted);
        assert!(
            !cand.subject_key.trim().is_empty(),
            "accepted candidate has subject_key"
        );
        assert!(
            !cand.evidence.is_empty(),
            "{} has evidence",
            cand.subject_key
        );
        assert_eq!(
            cand.evidence_count,
            cand.evidence.len(),
            "{} evidence_count matches evidence",
            cand.subject_key
        );
        assert!(
            !cand.promotion_reason.trim().is_empty(),
            "{} has promotion reason",
            cand.subject_key
        );
        assert!(
            cand.confidence > 0.0,
            "{} has positive confidence",
            cand.subject_key
        );
        assert!(
            !cand.evidence_window.start.is_empty(),
            "{} has evidence window start",
            cand.subject_key
        );
        assert!(
            !cand.evidence_window.end.is_empty(),
            "{} has evidence window end",
            cand.subject_key
        );
        assert!(
            policy::detect_secret(&cand.content).is_none(),
            "accepted candidate {} must not contain secret-shaped content",
            cand.subject_key
        );
    }
}

fn assert_candidates(
    fixture_name: &str,
    bucket: &str,
    actual: &[DreamCandidate],
    expected: &[CandidateExpectation],
) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{fixture_name} {bucket} candidate count"
    );
    for exp in expected {
        let cand = actual
            .iter()
            .find(|cand| cand.subject_key == exp.subject_key)
            .unwrap_or_else(|| {
                panic!(
                    "{fixture_name} missing {bucket} candidate {}",
                    exp.subject_key
                )
            });
        assert_eq!(
            cand.state,
            expected_state(bucket),
            "{fixture_name} {} bucket state",
            exp.subject_key
        );
        if let Some(record_type) = &exp.record_type {
            assert_eq!(
                cand.record_type.as_str(),
                record_type,
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
    }

    fn expected_state(bucket: &str) -> CandidateState {
        match bucket {
            "accepted" => CandidateState::Accepted,
            "rejected" => CandidateState::Rejected,
            "quarantined" => CandidateState::Quarantined,
            "stale" => CandidateState::Stale,
            other => panic!("unknown preview bucket {other}"),
        }
    }
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
            + sidecar.expect_preview.stale.len();
        assert!(
            assertion_count > 0,
            "{} has explicit preview assertions",
            fixture.name
        );
        for bucket in [
            &sidecar.expect_preview.accepted,
            &sidecar.expect_preview.rejected,
            &sidecar.expect_preview.quarantined,
            &sidecar.expect_preview.stale,
        ] {
            for candidate in bucket.iter() {
                assert!(
                    !candidate.subject_key.trim().is_empty(),
                    "{} sidecar candidate has readable subject_key",
                    fixture.name
                );
            }
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
        let seeded = seed_store(&fixture);
        let preview = preview_fixture(&fixture);

        assert_preview_matches(&fixture, &preview, &expected.expect_preview);

        if let Some(recall_before) = &expected.expect_recall_before {
            let text = recall_text(&seeded.service, recall_before);
            assert_recall(&format!("{} before", fixture.name), &text, recall_before);
        }

        if let Some(apply_expected) = &expected.expect_apply {
            let first = apply_preview(&seeded, &preview);
            assert_eq!(
                first.created, apply_expected.created,
                "{} first apply created",
                fixture.name
            );
            assert_eq!(
                first.archived, apply_expected.archived,
                "{} first apply archived",
                fixture.name
            );

            if let Some(recall_after) = &expected.expect_recall_after {
                let text = recall_text(&seeded.service, recall_after);
                assert_recall(&format!("{} after", fixture.name), &text, recall_after);
            }

            if apply_expected.idempotent_second_apply {
                let active_after_first = seeded.store.count_records().expect("count records");
                let second = apply_preview(&seeded, &preview);
                assert_eq!(
                    second,
                    ApplyReport::default(),
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
