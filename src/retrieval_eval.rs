//! Long-history retrieval quality eval loop (issue #153).
//!
//! This suite is deterministic and fixture-only: no external services, no
//! embeddings, no graph, and no model judge. It measures where current recall
//! loses evidence so later feature work can be prioritized from checked-in data.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Config;
use crate::domain::{Portability, RecordType, RepoIdentity, Scope, Sensitivity};
use crate::error::{Error, Result};
use crate::ids;
use crate::protocol::{AdapterExportRequest, RecallRequest};
use crate::service::Service;
use crate::store::{NewRecord, Store};

const FIXTURE_JSON: &str = include_str!("../tests/fixtures/retrieval/long_history.json");
const SUITE: &str = "retrieval_quality";
const TOP_K: usize = 5;

#[derive(Debug, Clone, Deserialize)]
struct RetrievalFixture {
    version: u32,
    records: Vec<FixtureRecord>,
    questions: Vec<FixtureQuestion>,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureRecord {
    id: String,
    family: String,
    profile: String,
    workspace: String,
    repo: Option<String>,
    #[serde(rename = "type")]
    record_type: String,
    scope: String,
    content: String,
    observed_at: String,
    confidence: f64,
    #[serde(default)]
    related_files: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    source_ids: Vec<String>,
    #[serde(default)]
    subject_key: Option<String>,
    #[serde(default)]
    episode_key: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    marker_operational_valence: Option<String>,
    #[serde(default)]
    marker_intensity: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureQuestion {
    id: String,
    family: String,
    query: String,
    profile: String,
    workspace: String,
    repo: Option<String>,
    #[serde(default)]
    files: Vec<String>,
    pack_mode: String,
    expected_record_ids: Vec<String>,
    answer_markers: Vec<String>,
    #[serde(default)]
    subject_key: Option<String>,
    #[serde(default)]
    episode_key: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RetrievalEvalReport {
    pub suite: &'static str,
    pub version: u32,
    pub status: &'static str,
    pub fixture: &'static str,
    pub fixture_families: Vec<String>,
    pub question_count: usize,
    pub baselines: Vec<RetrievalBaselineResult>,
    pub ranking_ablations: Vec<RankingAblationResult>,
    pub regression_fixtures: Vec<RegressionFixture>,
    pub next_recommended_ranking_changes: Vec<String>,
    pub notes: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct RetrievalBaselineResult {
    pub name: &'static str,
    pub description: &'static str,
    pub recall_at_k: f64,
    pub precision_at_k: f64,
    pub evidence_coverage: f64,
    pub context_bytes: usize,
    pub estimated_tokens: usize,
    pub latency_estimate_units: usize,
    pub cross_profile_leak: bool,
    pub failed_queries: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RankingAblationResult {
    pub name: &'static str,
    pub disabled_signals: Vec<&'static str>,
    pub recall_at_k: f64,
    pub precision_at_k: f64,
    pub evidence_coverage: f64,
    pub delta_vs_all_signals: f64,
    pub failed_queries: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RegressionFixture {
    pub query_id: String,
    pub family: String,
    pub baseline: String,
    pub reason: String,
    pub expected_record_ids: Vec<String>,
}

struct RetrievedItem {
    fixture_id: Option<String>,
    content: String,
    profile: String,
}

#[derive(Clone, Copy)]
struct SignalConfig {
    recency: bool,
    type_weight: bool,
    evidence_coverage: bool,
    subject_episode_match: bool,
    procedure_valence: bool,
    freshness: bool,
}

impl SignalConfig {
    const ALL: SignalConfig = SignalConfig {
        recency: true,
        type_weight: true,
        evidence_coverage: true,
        subject_episode_match: true,
        procedure_valence: true,
        freshness: true,
    };
}

pub fn run_retrieval_eval() -> Result<RetrievalEvalReport> {
    let fixture = load_fixture()?;
    let service = eval_service(&fixture)?;
    seed_fixture_records(&service, &fixture)?;

    let baselines = vec![
        score_raw_chronological(&fixture),
        score_keyword(&fixture),
        score_full_list(&fixture),
        score_memoryd_recall(&service, &fixture)?,
        score_context_pack(&service, &fixture)?,
        score_verbatim_evidence(&fixture),
    ];

    let all_signals = score_ablation("all_signals", vec![], SignalConfig::ALL, &fixture);
    let all_recall = all_signals.recall_at_k;
    let mut ranking_ablations = vec![all_signals];
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_recency",
            vec!["recency"],
            SignalConfig {
                recency: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_type_weight",
            vec!["type_weight"],
            SignalConfig {
                type_weight: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_evidence_coverage",
            vec!["evidence_coverage"],
            SignalConfig {
                evidence_coverage: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_subject_episode_match",
            vec!["subject_episode_match"],
            SignalConfig {
                subject_episode_match: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_procedure_valence",
            vec!["procedure_valence"],
            SignalConfig {
                procedure_valence: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));
    ranking_ablations.push(with_delta(
        score_ablation(
            "without_freshness",
            vec!["freshness"],
            SignalConfig {
                freshness: false,
                ..SignalConfig::ALL
            },
            &fixture,
        ),
        all_recall,
    ));

    let regression_fixtures = regression_fixtures(&fixture, &baselines, &ranking_ablations);
    let next_recommended_ranking_changes =
        next_recommended_ranking_changes(&baselines, &ranking_ablations);

    Ok(RetrievalEvalReport {
        suite: SUITE,
        version: fixture.version,
        status: "pass",
        fixture: "tests/fixtures/retrieval/long_history.json",
        fixture_families: fixture_families(&fixture),
        question_count: fixture.questions.len(),
        baselines,
        ranking_ablations,
        regression_fixtures,
        next_recommended_ranking_changes,
        notes: vec![
            "fixture-only deterministic scores; no external service calls",
            "recall_not_authority remains intact; eval output is measurement, not authority",
            "no graph, embeddings, or benchmark superiority claim in this MVP",
        ],
    })
}

pub fn render_retrieval_summary(report: &RetrievalEvalReport) -> String {
    let mut out = format!(
        "codex-memoryd retrieval quality eval: {}\nlong-history questions: {}\nfixture: {}\n\n",
        report.status, report.question_count, report.fixture
    );
    out.push_str(&format!(
        "{:<20} {:>9} {:>10} {:>9} {:>9} {:>8}\n",
        "baseline", "recall@k", "prec@k", "coverage", "tokens", "failures"
    ));
    for baseline in &report.baselines {
        out.push_str(&format!(
            "{:<20} {:>9.2} {:>10.2} {:>9.2} {:>9} {:>8}\n",
            baseline.name,
            baseline.recall_at_k,
            baseline.precision_at_k,
            baseline.evidence_coverage,
            baseline.estimated_tokens,
            baseline.failed_queries.len()
        ));
    }
    out.push_str("\nranking ablations\n");
    for ablation in &report.ranking_ablations {
        out.push_str(&format!(
            "{:<30} recall@k {:.2} delta {:+.2} failures {}\n",
            ablation.name,
            ablation.recall_at_k,
            ablation.delta_vs_all_signals,
            ablation.failed_queries.len()
        ));
    }
    out.push_str("\nnext ranking changes\n");
    for item in &report.next_recommended_ranking_changes {
        out.push_str("- ");
        out.push_str(item);
        out.push('\n');
    }
    out
}

fn load_fixture() -> Result<RetrievalFixture> {
    serde_json::from_str(FIXTURE_JSON)
        .map_err(|e| Error::internal(format!("parse retrieval fixture: {e}")))
}

fn eval_service(fixture: &RetrievalFixture) -> Result<Service> {
    let store = Store::open(":memory:")?;
    let default_workspace = fixture
        .questions
        .first()
        .map(|q| q.workspace.clone())
        .unwrap_or_else(|| "retrieval-lab".to_string());
    let config = Config {
        default_workspace,
        ..Default::default()
    };
    Ok(Service::new(store, config))
}

fn seed_fixture_records(service: &Service, fixture: &RetrievalFixture) -> Result<()> {
    let mut workspaces = BTreeSet::new();
    for record in &fixture.records {
        workspaces.insert((record.profile.as_str(), record.workspace.as_str()));
    }
    for (profile, workspace) in workspaces {
        service.store.ensure_workspace(profile, workspace)?;
    }

    for record in &fixture.records {
        let record_type = RecordType::parse(&record.record_type).ok_or_else(|| {
            Error::invalid_request(format!(
                "retrieval fixture record {} has unknown type {}",
                record.id, record.record_type
            ))
        })?;
        let scope = Scope::parse(&record.scope).ok_or_else(|| {
            Error::invalid_request(format!(
                "retrieval fixture record {} has unknown scope {}",
                record.id, record.scope
            ))
        })?;
        let sensitivity = if record.profile == "work" {
            Sensitivity::WorkConfidential
        } else {
            Sensitivity::Personal
        };
        let mut source_ids = record.source_ids.clone();
        source_ids.push(format!("fixture:{}", record.id));
        service.store.upsert_record(&NewRecord {
            profile_id: record.profile.clone(),
            workspace_id: record.workspace.clone(),
            repo_id: record.repo.clone(),
            subject_id: None,
            episode_id: None,
            scope,
            record_type,
            content: record.content.clone(),
            related_files: record.related_files.clone(),
            tags: record.tags.clone(),
            sensitivity,
            portability: Portability::Portable,
            confidence: record.confidence,
            source_ids,
            content_hash: ids::content_hash(
                &record.profile,
                &record.workspace,
                record.repo.as_deref(),
                record_type.as_str(),
                scope.as_str(),
                &record.content,
            ),
            supersedes: record.supersedes.clone(),
            metadata: json!({
                "fixture_family": record.family,
                "fixture_id": record.id,
                "observed_at": record.observed_at,
                "subject_key": record.subject_key,
                "episode_key": record.episode_key,
                "state": record.state,
                "marker_operational_valence": record.marker_operational_valence,
                "marker_intensity": record.marker_intensity,
                "origin": "retrieval_eval_fixture"
            }),
        })?;
    }
    Ok(())
}

fn score_raw_chronological(fixture: &RetrievalFixture) -> RetrievalBaselineResult {
    score_baseline(
        "raw_chronological",
        "all records in observed order, including wrong profiles",
        fixture,
        |fixture, _question| {
            let mut records = fixture.records.iter().collect::<Vec<_>>();
            records.sort_by(|a, b| {
                a.observed_at
                    .cmp(&b.observed_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            records
                .into_iter()
                .take(TOP_K)
                .map(record_item)
                .collect::<Vec<RetrievedItem>>()
        },
    )
}

fn score_keyword(fixture: &RetrievalFixture) -> RetrievalBaselineResult {
    score_baseline(
        "keyword_search",
        "lexical overlap only, no temporal or subject signals",
        fixture,
        |fixture, question| {
            let mut rows = fixture
                .records
                .iter()
                .map(|record| (lexical_overlap(&question.query, &record.content), record))
                .filter(|(score, _)| *score > 0.0)
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.1.observed_at.cmp(&a.1.observed_at))
                    .then_with(|| a.1.id.cmp(&b.1.id))
            });
            rows.into_iter()
                .take(TOP_K)
                .map(|(_, record)| record_item(record))
                .collect::<Vec<RetrievedItem>>()
        },
    )
}

fn score_full_list(fixture: &RetrievalFixture) -> RetrievalBaselineResult {
    score_baseline(
        "full_list",
        "all profile/workspace records in observed order",
        fixture,
        |fixture, question| {
            fixture
                .records
                .iter()
                .filter(|record| {
                    record.profile == question.profile && record.workspace == question.workspace
                })
                .take(TOP_K)
                .map(record_item)
                .collect::<Vec<RetrievedItem>>()
        },
    )
}

fn score_memoryd_recall(
    service: &Service,
    fixture: &RetrievalFixture,
) -> Result<RetrievalBaselineResult> {
    let mut by_question = BTreeMap::new();
    for question in &fixture.questions {
        let recall = service.recall(RecallRequest {
            profile: Some(question.profile.clone()),
            workspace: Some(question.workspace.clone()),
            repo: question.repo.as_deref().map(repo_identity),
            session: None,
            query: Some(question.query.clone()),
            files: question.files.clone(),
            max_tokens: Some(900),
            pack_mode: Some(question.pack_mode.clone()),
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            metadata: None,
        })?;
        let items = recall
            .facts
            .into_iter()
            .take(TOP_K)
            .map(|fact| {
                let fixture_id = fact
                    .policy
                    .provenance
                    .evidence_refs
                    .iter()
                    .find_map(|id| id.strip_prefix("fixture:").map(str::to_string));
                RetrievedItem {
                    fixture_id,
                    content: fact.content,
                    profile: fact.policy.provenance.profile_id,
                }
            })
            .collect::<Vec<_>>();
        by_question.insert(question.id.clone(), items);
    }
    Ok(score_precomputed(
        "memoryd_recall",
        "current scoped recall ranking and policy gates",
        fixture,
        by_question,
    ))
}

fn score_context_pack(
    service: &Service,
    fixture: &RetrievalFixture,
) -> Result<RetrievalBaselineResult> {
    let mut by_question = BTreeMap::new();
    let mut cache = BTreeMap::new();
    for question in &fixture.questions {
        let key = format!("{}:{}", question.profile, question.workspace);
        if !cache.contains_key(&key) {
            let export = service.adapter_export(AdapterExportRequest {
                profile: Some(question.profile.clone()),
                workspace: Some(question.workspace.clone()),
                target: "mcp-pack".to_string(),
                subject_id: None,
                max_bytes: Some(20_000),
            })?;
            let items = export
                .context_pack
                .map(|pack| {
                    pack.records
                        .into_iter()
                        .take(TOP_K)
                        .map(|record| RetrievedItem {
                            fixture_id: fixture_id_for_content(fixture, &record.content),
                            content: record.content,
                            profile: question.profile.clone(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            cache.insert(key.clone(), items);
        }
        by_question.insert(
            question.id.clone(),
            clone_items(cache.get(&key).unwrap_or(&Vec::new())),
        );
    }
    Ok(score_precomputed(
        "context_pack",
        "adapter context pack without query-specific reranking",
        fixture,
        by_question,
    ))
}

fn score_verbatim_evidence(fixture: &RetrievalFixture) -> RetrievalBaselineResult {
    score_baseline(
        "verbatim_evidence",
        "oracle exact evidence excerpts from checked-in fixture records",
        fixture,
        |fixture, question| {
            fixture
                .records
                .iter()
                .filter(|record| {
                    record.profile == question.profile
                        && question.expected_record_ids.contains(&record.id)
                })
                .map(record_item)
                .collect::<Vec<_>>()
        },
    )
}

fn score_baseline<F>(
    name: &'static str,
    description: &'static str,
    fixture: &RetrievalFixture,
    retrieve: F,
) -> RetrievalBaselineResult
where
    F: Fn(&RetrievalFixture, &FixtureQuestion) -> Vec<RetrievedItem>,
{
    let mut by_question = BTreeMap::new();
    for question in &fixture.questions {
        by_question.insert(question.id.clone(), retrieve(fixture, question));
    }
    score_precomputed(name, description, fixture, by_question)
}

fn score_precomputed(
    name: &'static str,
    description: &'static str,
    fixture: &RetrievalFixture,
    by_question: BTreeMap<String, Vec<RetrievedItem>>,
) -> RetrievalBaselineResult {
    let mut recall_sum = 0.0;
    let mut precision_sum = 0.0;
    let mut coverage_sum = 0.0;
    let mut context_bytes = 0;
    let mut latency_estimate_units = 0;
    let mut cross_profile_leak = false;
    let mut failed_queries = Vec::new();

    for question in &fixture.questions {
        let empty = Vec::new();
        let items = by_question.get(&question.id).unwrap_or(&empty);
        let ids = items
            .iter()
            .filter_map(|item| item.fixture_id.as_deref())
            .collect::<BTreeSet<_>>();
        let relevant = question
            .expected_record_ids
            .iter()
            .filter(|id| ids.contains(id.as_str()))
            .count();
        let marker_hits = question
            .answer_markers
            .iter()
            .filter(|marker| {
                items
                    .iter()
                    .any(|item| item.content.contains(marker.as_str()))
            })
            .count();
        let hit = relevant == question.expected_record_ids.len()
            || marker_hits == question.answer_markers.len();
        if hit {
            recall_sum += 1.0;
        } else {
            failed_queries.push(question.id.clone());
        }
        precision_sum += if items.is_empty() {
            0.0
        } else {
            items
                .iter()
                .filter(|item| {
                    item.fixture_id
                        .as_ref()
                        .is_some_and(|id| question.expected_record_ids.contains(id))
                })
                .count() as f64
                / items.len() as f64
        };
        coverage_sum += marker_hits as f64 / question.answer_markers.len().max(1) as f64;
        context_bytes += items.iter().map(|item| item.content.len()).sum::<usize>();
        latency_estimate_units += items.len();
        cross_profile_leak = cross_profile_leak
            || items
                .iter()
                .any(|item| item.profile.as_str() != question.profile.as_str());
    }

    let count = fixture.questions.len().max(1) as f64;
    RetrievalBaselineResult {
        name,
        description,
        recall_at_k: recall_sum / count,
        precision_at_k: precision_sum / count,
        evidence_coverage: coverage_sum / count,
        context_bytes,
        estimated_tokens: estimate_tokens(context_bytes),
        latency_estimate_units,
        cross_profile_leak,
        failed_queries,
    }
}

fn score_ablation(
    name: &'static str,
    disabled_signals: Vec<&'static str>,
    config: SignalConfig,
    fixture: &RetrievalFixture,
) -> RankingAblationResult {
    let mut by_question = BTreeMap::new();
    for question in &fixture.questions {
        let mut scored = fixture
            .records
            .iter()
            .filter(|record| {
                record.profile == question.profile && record.workspace == question.workspace
            })
            .map(|record| (fixture_score(record, question, config), record))
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.1.observed_at.cmp(&a.1.observed_at))
                .then_with(|| a.1.id.cmp(&b.1.id))
        });
        by_question.insert(
            question.id.clone(),
            scored
                .into_iter()
                .take(TOP_K)
                .map(|(_, record)| record_item(record))
                .collect::<Vec<_>>(),
        );
    }
    let scored = score_precomputed(name, "ranking signal ablation", fixture, by_question);
    RankingAblationResult {
        name,
        disabled_signals,
        recall_at_k: scored.recall_at_k,
        precision_at_k: scored.precision_at_k,
        evidence_coverage: scored.evidence_coverage,
        delta_vs_all_signals: 0.0,
        failed_queries: scored.failed_queries,
    }
}

fn with_delta(mut ablation: RankingAblationResult, all_recall: f64) -> RankingAblationResult {
    ablation.delta_vs_all_signals = ablation.recall_at_k - all_recall;
    ablation
}

fn fixture_score(record: &FixtureRecord, question: &FixtureQuestion, config: SignalConfig) -> f64 {
    let mut score = 1.0 + lexical_overlap(&question.query, &record.content) * 2.0;
    if config.recency {
        score += recency_score(&record.observed_at);
    }
    if config.type_weight {
        score += RecordType::parse(&record.record_type)
            .map(|t| t.recall_weight() * 1.5)
            .unwrap_or(0.0);
    }
    if config.evidence_coverage {
        score += question
            .answer_markers
            .iter()
            .filter(|marker| record.content.contains(marker.as_str()))
            .count() as f64
            * 3.0;
    }
    if config.subject_episode_match {
        if record.subject_key == question.subject_key {
            score += 2.0;
        }
        if record.episode_key == question.episode_key {
            score += 1.0;
        }
    }
    if config.procedure_valence && record.marker_operational_valence.is_some() {
        score += record.marker_intensity.unwrap_or(0.0).clamp(0.0, 1.0) * 0.5;
    }
    if config.freshness {
        match record.state.as_deref() {
            Some("current") => score += 1.5,
            Some("superseded") => score -= 4.0,
            _ => {}
        }
    }
    score
}

fn lexical_overlap(query: &str, content: &str) -> f64 {
    let content = content.to_ascii_lowercase();
    let words = query
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| word.len() >= 4)
        .map(|word| word.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if words.is_empty() {
        return 0.0;
    }
    let hits = words
        .iter()
        .filter(|word| content.contains(word.as_str()))
        .count();
    hits as f64 / words.len() as f64
}

fn recency_score(observed_at: &str) -> f64 {
    match observed_at {
        ts if ts >= "2026-06-10" => 1.2,
        ts if ts >= "2026-06-01" => 0.8,
        ts if ts >= "2026-05-01" => 0.4,
        _ => 0.0,
    }
}

fn record_item(record: &FixtureRecord) -> RetrievedItem {
    RetrievedItem {
        fixture_id: Some(record.id.clone()),
        content: record.content.clone(),
        profile: record.profile.clone(),
    }
}

fn fixture_id_for_content(fixture: &RetrievalFixture, content: &str) -> Option<String> {
    fixture
        .records
        .iter()
        .find(|record| record.content == content)
        .map(|record| record.id.clone())
}

fn clone_items(items: &[RetrievedItem]) -> Vec<RetrievedItem> {
    items
        .iter()
        .map(|item| RetrievedItem {
            fixture_id: item.fixture_id.clone(),
            content: item.content.clone(),
            profile: item.profile.clone(),
        })
        .collect()
}

fn estimate_tokens(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

fn fixture_families(fixture: &RetrievalFixture) -> Vec<String> {
    let mut families = fixture
        .questions
        .iter()
        .map(|question| question.family.clone())
        .collect::<BTreeSet<_>>();
    let preferred = [
        "single_hop",
        "temporal",
        "contradiction",
        "preference_drift",
        "multi_hop",
        "open_domain",
    ];
    let mut ordered = Vec::new();
    for family in preferred {
        if let Some(value) = families.take(family) {
            ordered.push(value);
        }
    }
    ordered.extend(families);
    ordered
}

fn repo_identity(repo_id: &str) -> RepoIdentity {
    RepoIdentity {
        repo_id: repo_id.to_string(),
        ..Default::default()
    }
}

fn regression_fixtures(
    fixture: &RetrievalFixture,
    baselines: &[RetrievalBaselineResult],
    ablations: &[RankingAblationResult],
) -> Vec<RegressionFixture> {
    let mut out = Vec::new();
    for baseline in baselines {
        for query_id in &baseline.failed_queries {
            if let Some(question) = fixture.questions.iter().find(|q| &q.id == query_id) {
                out.push(RegressionFixture {
                    query_id: question.id.clone(),
                    family: question.family.clone(),
                    baseline: baseline.name.to_string(),
                    reason: "missed expected fixture evidence".to_string(),
                    expected_record_ids: question.expected_record_ids.clone(),
                });
            }
        }
    }
    for ablation in ablations {
        if ablation.name == "all_signals" {
            continue;
        }
        for query_id in &ablation.failed_queries {
            if let Some(question) = fixture.questions.iter().find(|q| &q.id == query_id) {
                out.push(RegressionFixture {
                    query_id: question.id.clone(),
                    family: question.family.clone(),
                    baseline: ablation.name.to_string(),
                    reason: format!(
                        "ranking ablation failed without {:?}",
                        ablation.disabled_signals
                    ),
                    expected_record_ids: question.expected_record_ids.clone(),
                });
            }
        }
    }
    out
}

fn next_recommended_ranking_changes(
    baselines: &[RetrievalBaselineResult],
    ablations: &[RankingAblationResult],
) -> Vec<String> {
    let memoryd = baselines.iter().find(|b| b.name == "memoryd_recall");
    let context = baselines.iter().find(|b| b.name == "context_pack");
    let all = ablations.iter().find(|a| a.name == "all_signals");
    let without_subject = ablations
        .iter()
        .find(|a| a.name == "without_subject_episode_match");
    let without_freshness = ablations.iter().find(|a| a.name == "without_freshness");
    let without_evidence = ablations
        .iter()
        .find(|a| a.name == "without_evidence_coverage");

    let mut recommendations = Vec::new();
    if memoryd.is_some_and(|b| b.recall_at_k < 1.0) {
        recommendations.push(
            "Current memoryd recall misses at least one long-history query; inspect regression_fixtures before #154-#156."
                .to_string(),
        );
    }
    if without_subject
        .zip(all)
        .is_some_and(|(ablation, all)| ablation.recall_at_k < all.recall_at_k)
    {
        recommendations.push(
            "Subject/episode match improves fixture recall; prioritize #154 if these failures dominate."
                .to_string(),
        );
    } else {
        recommendations.push(
            "Keep subject/episode match as a ranking signal candidate; #154 should start with fixture-backed aliases, not graph storage."
                .to_string(),
        );
    }
    if without_freshness
        .zip(all)
        .is_some_and(|(ablation, all)| ablation.recall_at_k < all.recall_at_k)
    {
        recommendations.push(
            "Freshness/current-state signals protect temporal and contradiction answers; prioritize #155 if misses cluster there."
                .to_string(),
        );
    }
    if without_evidence
        .zip(all)
        .is_some_and(|(ablation, all)| ablation.recall_at_k < all.recall_at_k)
    {
        recommendations.push(
            "Evidence coverage is useful in this fixture; add non-oracle coverage proxies before changing weights."
                .to_string(),
        );
    }
    if context.is_some_and(|b| b.recall_at_k < memoryd.map_or(0.0, |m| m.recall_at_k)) {
        recommendations.push(
            "Context packs need query-aware reranking before becoming competitive recall evidence."
                .to_string(),
        );
    }
    if recommendations.is_empty() {
        recommendations.push(
            "No dominant miss class yet; expand fixture families before ranking changes."
                .to_string(),
        );
    }
    recommendations
}
