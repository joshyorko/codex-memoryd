use std::collections::BTreeSet;
use std::path::Path;

use codex_memoryd::domain::Portability;
use codex_memoryd::domain::Profile;
use codex_memoryd::domain::RecordType;
use codex_memoryd::domain::Scope;
use codex_memoryd::domain::Sensitivity;
use codex_memoryd::domain::TemporalState;
use codex_memoryd::ids;
use codex_memoryd::recall;
use codex_memoryd::recall::RecallParams;
use codex_memoryd::store::NewRecord;
use codex_memoryd::store::Store;
use rusqlite::params;
use rusqlite::Connection;
use serde::Deserialize;
use tempfile::TempDir;

const PROFILE: &str = "personal";
const WORKSPACE: &str = "temporal-fixtures";

#[derive(Debug, Deserialize)]
struct TemporalFixture {
    scenario: String,
    now: String,
    records: Vec<FixtureRecord>,
    queries: Vec<FixtureQuery>,
}

#[derive(Debug, Deserialize)]
struct FixtureRecord {
    id: String,
    content: String,
    #[serde(rename = "type")]
    record_type: String,
    #[serde(default)]
    temporal_state: Option<String>,
    #[serde(default)]
    valid_from: Option<String>,
    #[serde(default)]
    valid_until: Option<String>,
    #[serde(default)]
    observed_at: Option<String>,
    #[serde(default)]
    invalidated_at: Option<String>,
    #[serde(default)]
    superseded_by: Option<String>,
    #[serde(default)]
    historical_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FixtureQuery {
    name: String,
    #[serde(default)]
    as_of: Option<String>,
    expect_visible: Vec<String>,
    #[serde(default)]
    expect_withheld_reasons: Vec<String>,
}

fn load_fixture(path: &str) -> TemporalFixture {
    let raw = std::fs::read_to_string(path).expect("read fixture");
    serde_json::from_str(&raw).expect("parse temporal fixture")
}

fn seeded_fixture_store(path: &Path, fixture: &TemporalFixture) -> Store {
    let store = Store::open(path).expect("open store");
    store
        .ensure_workspace(PROFILE, WORKSPACE)
        .expect("workspace");

    let conn = Connection::open(path).expect("open raw sqlite");
    for record in &fixture.records {
        insert_fixture_record(&conn, &fixture.scenario, record);
    }

    store
}

fn insert_fixture_record(conn: &Connection, scenario: &str, record: &FixtureRecord) {
    let created_at = record
        .observed_at
        .as_deref()
        .or(record.valid_from.as_deref())
        .unwrap_or("2026-01-01T00:00:00Z");
    let record_type = RecordType::parse(&record.record_type)
        .unwrap_or_else(|| panic!("unknown record type {}", record.record_type));
    let content_hash = ids::content_hash(
        PROFILE,
        WORKSPACE,
        None,
        record_type.as_str(),
        "workspace",
        &format!("{scenario}:{}", record.id),
    );
    let temporal_state = record.temporal_state.as_deref().unwrap_or("current");

    conn.execute(
        "INSERT INTO memory_records(
            id, profile_id, workspace_id, repo_id, subject_id, episode_id,
            scope, type, content, related_files, tags, sensitivity, portability,
            confidence, source_ids, content_hash, supersedes, created_at, updated_at,
            last_used_at, archived, trust_state, trust_score, quarantine_reason,
            quarantined_at, promoted_at, valid_from, valid_until, observed_at,
            invalidated_at, superseded_by, historical_reason, temporal_state, metadata
        ) VALUES (
            ?1, ?2, ?3, NULL, NULL, NULL,
            'workspace', ?4, ?5, '[]', '[]', 'personal', 'portable',
            0.9, '[]', ?6, '[]', ?7, ?7,
            NULL, 0, 'trusted', 1.0, NULL,
            NULL, NULL, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14, '{}'
        )",
        params![
            record.id,
            PROFILE,
            WORKSPACE,
            record_type.as_str(),
            record.content,
            content_hash,
            created_at,
            record.valid_from,
            record.valid_until,
            record.observed_at,
            record.invalidated_at,
            record.superseded_by,
            record.historical_reason,
            temporal_state,
        ],
    )
    .expect("insert fixture record");
}

fn recall_ids(store: &Store, fixture: &TemporalFixture, query: &FixtureQuery) -> BTreeSet<String> {
    let include_types: Vec<RecordType> = Vec::new();
    let exclude_types: Vec<RecordType> = Vec::new();
    let files: Vec<String> = Vec::new();
    let params = RecallParams {
        profile: Profile::Personal,
        workspace: WORKSPACE,
        repo: None,
        query: "",
        files: &files,
        max_tokens: 4096,
        pack_mode: "default",
        include_types: &include_types,
        exclude_types: &exclude_types,
        recency_days: None,
        now: Some(fixture.now.as_str()),
        as_of: query.as_of.as_deref(),
        include_history: false,
    };
    let response = recall::recall(store, &params).expect("recall");
    response.facts.into_iter().map(|fact| fact.id).collect()
}

fn recall_withheld_reasons(
    store: &Store,
    fixture: &TemporalFixture,
    query: &FixtureQuery,
) -> BTreeSet<String> {
    let include_types: Vec<RecordType> = Vec::new();
    let exclude_types: Vec<RecordType> = Vec::new();
    let files: Vec<String> = Vec::new();
    let params = RecallParams {
        profile: Profile::Personal,
        workspace: WORKSPACE,
        repo: None,
        query: "",
        files: &files,
        max_tokens: 4096,
        pack_mode: "default",
        include_types: &include_types,
        exclude_types: &exclude_types,
        recency_days: None,
        now: Some(fixture.now.as_str()),
        as_of: query.as_of.as_deref(),
        include_history: false,
    };
    let response = recall::recall(store, &params).expect("recall");
    response
        .withheld
        .into_iter()
        .map(|withheld| withheld.reason)
        .collect()
}

fn new_record(content: &str, marker: &str) -> NewRecord {
    NewRecord {
        profile_id: PROFILE.to_string(),
        workspace_id: WORKSPACE.to_string(),
        repo_id: None,
        subject_id: None,
        episode_id: None,
        scope: Scope::Workspace,
        record_type: RecordType::Preference,
        content: content.to_string(),
        related_files: vec![],
        tags: vec![],
        sensitivity: Sensitivity::Personal,
        portability: Portability::Portable,
        confidence: 0.9,
        source_ids: vec![],
        content_hash: ids::content_hash(
            PROFILE,
            WORKSPACE,
            None,
            "preference",
            "workspace",
            marker,
        ),
        supersedes: vec![],
        metadata: serde_json::json!({}),
    }
}

fn assert_fixture(path: &str) {
    let fixture = load_fixture(path);
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("temporal.db");
    let store = seeded_fixture_store(&db, &fixture);

    for query in &fixture.queries {
        let actual = recall_ids(&store, &fixture, query);
        let expected: BTreeSet<String> = query.expect_visible.iter().cloned().collect();
        assert_eq!(
            actual, expected,
            "{} / {} visible ids",
            fixture.scenario, query.name
        );

        let withheld = recall_withheld_reasons(&store, &fixture, query);
        for reason in &query.expect_withheld_reasons {
            assert!(
                withheld.contains(reason),
                "{} / {} missing withheld reason {reason}; got {:?}",
                fixture.scenario,
                query.name,
                withheld
            );
        }
    }
}

#[test]
fn supersede_record_retires_old_record_and_links_successor() {
    let store = Store::open(":memory:").unwrap();
    store.ensure_workspace(PROFILE, WORKSPACE).unwrap();
    let old_id = store
        .upsert_record(&new_record(
            "Indent with spaces in this workspace.",
            "old-pref",
        ))
        .unwrap()
        .id()
        .to_string();
    let new_id = store
        .upsert_record(&new_record(
            "Indent with tabs in this workspace.",
            "new-pref",
        ))
        .unwrap()
        .id()
        .to_string();
    let now = "2026-04-01T00:00:00Z";

    let old = store
        .supersede_record(PROFILE, WORKSPACE, &old_id, &new_id, "superseded", now)
        .unwrap()
        .unwrap();
    let new = store.get_record(&new_id).unwrap().unwrap();

    assert_eq!(old.temporal_state, TemporalState::Superseded);
    assert_eq!(old.superseded_by.as_deref(), Some(new_id.as_str()));
    assert_eq!(old.valid_until.as_deref(), Some(now));
    assert_eq!(old.historical_reason.as_deref(), Some("superseded"));
    assert!(new.supersedes.contains(&old_id));
}

#[test]
fn invalidate_record_hides_claim_from_default_recall_but_preserves_history() {
    let store = Store::open(":memory:").unwrap();
    store.ensure_workspace(PROFILE, WORKSPACE).unwrap();
    let id = store
        .upsert_record(&new_record(
            "The integration tests pass on main.",
            "claim-pass",
        ))
        .unwrap()
        .id()
        .to_string();
    let now = "2026-06-10T00:00:00Z";

    let invalidated = store
        .invalidate_record(PROFILE, WORKSPACE, &id, "contradicted", now)
        .unwrap()
        .unwrap();
    assert_eq!(invalidated.temporal_state, TemporalState::Invalidated);
    assert_eq!(invalidated.invalidated_at.as_deref(), Some(now));
    assert_eq!(
        invalidated.historical_reason.as_deref(),
        Some("contradicted")
    );

    let include_types: Vec<RecordType> = Vec::new();
    let exclude_types: Vec<RecordType> = Vec::new();
    let files: Vec<String> = Vec::new();
    let current = recall::recall(
        &store,
        &RecallParams {
            profile: Profile::Personal,
            workspace: WORKSPACE,
            repo: None,
            query: "integration tests",
            files: &files,
            max_tokens: 4096,
            pack_mode: "default",
            include_types: &include_types,
            exclude_types: &exclude_types,
            recency_days: None,
            now: Some("2026-06-14T00:00:00Z"),
            as_of: None,
            include_history: false,
        },
    )
    .unwrap();
    assert!(current.facts.is_empty());

    let history = recall::recall(
        &store,
        &RecallParams {
            profile: Profile::Personal,
            workspace: WORKSPACE,
            repo: None,
            query: "integration tests",
            files: &files,
            max_tokens: 4096,
            pack_mode: "default",
            include_types: &include_types,
            exclude_types: &exclude_types,
            recency_days: None,
            now: Some("2026-06-14T00:00:00Z"),
            as_of: None,
            include_history: true,
        },
    )
    .unwrap();
    assert_eq!(history.facts.len(), 1);
    assert_eq!(history.facts[0].id, id);
}

#[test]
fn temporal_fixtures_match_current_and_as_of_recall() {
    for path in [
        "tests/fixtures/temporal/backfill_default_current.json",
        "tests/fixtures/temporal/changed_preference.json",
        "tests/fixtures/temporal/repo_state_change.json",
        "tests/fixtures/temporal/completed_work.json",
        "tests/fixtures/temporal/contradicted_claim.json",
        "tests/fixtures/temporal/relative_time_record.json",
    ] {
        assert_fixture(path);
    }
}

#[test]
fn include_history_mode_keeps_old_evidence_inspectable() {
    let fixture = load_fixture("tests/fixtures/temporal/changed_preference.json");
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("temporal-history.db");
    let store = seeded_fixture_store(&db, &fixture);
    let include_types: Vec<RecordType> = Vec::new();
    let exclude_types: Vec<RecordType> = Vec::new();
    let files: Vec<String> = Vec::new();
    let params = RecallParams {
        profile: Profile::Personal,
        workspace: WORKSPACE,
        repo: None,
        query: "Indent",
        files: &files,
        max_tokens: 4096,
        pack_mode: "default",
        include_types: &include_types,
        exclude_types: &exclude_types,
        recency_days: None,
        now: Some(fixture.now.as_str()),
        as_of: None,
        include_history: true,
    };

    let ids: BTreeSet<String> = recall::recall(&store, &params)
        .expect("recall")
        .facts
        .into_iter()
        .map(|fact| fact.id)
        .collect();

    assert!(ids.contains("pref_spaces"));
    assert!(ids.contains("pref_tabs"));
}
