//! Recall ranking + context packing (SPEC §8) and explicit search (SPEC §6.3).
//!
//! Recall gathers candidate records filtered by profile/workspace, scores them
//! by repo/file/type/recency/confidence, packs the top results within a token
//! budget, attaches recent checkpoints + citations, and marks the context as
//! recall (not authority).

use std::collections::BTreeSet;
use time::format_description::well_known::Rfc3339;
use time::Duration;
use time::OffsetDateTime;

use crate::domain::MemoryRecord;
use crate::domain::Profile;
use crate::domain::RecordType;
use crate::domain::RepoIdentity;
use crate::error::Result;
use crate::protocol::Citation;
use crate::protocol::RecallAdmission;
use crate::protocol::RecallCheckpoint;
use crate::protocol::RecallFact;
use crate::protocol::RecallFactPolicy;
use crate::protocol::RecallFreshness;
use crate::protocol::RecallPack;
use crate::protocol::RecallPolicy;
use crate::protocol::RecallProvenance;
use crate::protocol::RecallResponse;
use crate::protocol::RecallWithheld;
use crate::protocol::SearchMatch;
use crate::protocol::SearchResponse;
use crate::store::RecordQuery;
use crate::store::Store;

/// Rough token estimate: ~4 chars per token. Conservative and deterministic.
fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}

/// Records older than this many days are considered stale for display hints.
const STALE_DAYS: i64 = 120;

/// Parameters resolved for a recall request.
pub struct RecallParams<'a> {
    pub profile: Profile,
    pub workspace: &'a str,
    pub repo: Option<&'a RepoIdentity>,
    pub query: &'a str,
    pub files: &'a [String],
    pub max_tokens: usize,
    pub pack_mode: &'a str,
    pub include_types: &'a [RecordType],
    pub exclude_types: &'a [RecordType],
    pub recency_days: Option<i64>,
}

/// A scored candidate, kept internal to ranking.
struct Scored {
    record: MemoryRecord,
    score: f64,
    ranking_signals: Vec<String>,
}

fn dedupe_ordered_signals(signals: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for signal in signals {
        if seen.insert(signal.clone()) {
            deduped.push(signal);
        }
    }
    deduped
}

fn build_withheld(
    archived: usize,
    secret_blocked: usize,
    type_filtered: usize,
    pack_withheld: usize,
) -> Vec<RecallWithheld> {
    let mut withheld = Vec::new();
    if archived > 0 {
        withheld.push(RecallWithheld {
            reason: "archived".to_string(),
            count: archived,
            gates: vec!["active_records".to_string()],
        });
    }
    if secret_blocked > 0 {
        withheld.push(RecallWithheld {
            reason: "secret_blocked".to_string(),
            count: secret_blocked,
            gates: vec!["secret_blocked".to_string()],
        });
    }
    if type_filtered > 0 {
        withheld.push(RecallWithheld {
            reason: "type_filtered".to_string(),
            count: type_filtered,
            gates: vec!["include_exclude_types".to_string()],
        });
    }
    if pack_withheld > 0 {
        withheld.push(RecallWithheld {
            reason: "pack_truncated".to_string(),
            count: pack_withheld,
            gates: vec!["max_tokens".to_string(), "result_limit".to_string()],
        });
    }
    withheld
}

/// Execute recall: gather, rank, pack, attach checkpoints + citations.
pub fn recall(store: &Store, params: &RecallParams) -> Result<RecallResponse> {
    let recency_cutoff = params.recency_days.and_then(rfc3339_cutoff);

    let filters = RecordQuery {
        profile_id: Some(params.profile.as_str().to_string()),
        workspace_id: Some(params.workspace.to_string()),
        repo_id: None, // repo is a ranking signal, not a hard filter
        record_type: None,
        scope: None,
        include_archived: false,
        recency_cutoff,
        // Gather a generous candidate pool; ranking + packing trims it.
        limit: 200,
        offset: 0,
    };
    let recall_omissions = store.recall_omission_counts(&filters, params.query)?;

    // Use search when a query is present; otherwise list candidates.
    let candidates = if params.query.trim().is_empty() {
        store.query_records(&filters)?
    } else {
        let mut hits = store.search_records(params.query, &filters)?;
        if hits.is_empty() {
            // Fall back to recent records so recall is useful even when the
            // query doesn't lexically match (SPEC §8: "useful recall").
            hits = store.query_records(&filters)?;
        }
        hits
    };

    let repo_id = params.repo.map(|r| r.repo_id.as_str());
    let mut type_filtered = 0usize;
    let mut scored: Vec<Scored> = candidates
        .into_iter()
        .filter_map(|record| {
            if !type_allowed(
                record.record_type,
                params.include_types,
                params.exclude_types,
            ) {
                type_filtered += 1;
                return None;
            }
            let score = score_record(
                &record,
                repo_id,
                params.files,
                params.query,
                params.pack_mode,
            );
            let ranking_signals = ranking_signals(
                &record,
                repo_id,
                params.files,
                params.query,
                params.pack_mode,
            );
            Some(Scored {
                record,
                score,
                ranking_signals,
            })
        })
        .collect();

    // Highest score first; tie-break on recency then confidence.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.record.updated_at.cmp(&a.record.updated_at))
            .then_with(|| {
                b.record
                    .confidence
                    .partial_cmp(&a.record.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.record.id.cmp(&b.record.id))
    });

    // Pack within the token budget.
    let mut facts: Vec<RecallFact> = Vec::new();
    let mut citations: Vec<Citation> = Vec::new();
    let mut touched: Vec<String> = Vec::new();
    let mut used_tokens = 0usize;
    let mut truncated = false;
    let mut pack_withheld = 0usize;
    let mut top_level_ranking_signals = Vec::new();
    let admission_gates = vec!["profile_workspace".to_string()];

    for entry in &scored {
        let r = &entry.record;
        let cost = estimate_tokens(&r.content);
        if used_tokens + cost > params.max_tokens && !facts.is_empty() {
            truncated = true;
            break;
        }
        used_tokens += cost;
        let stale = is_stale(&r.updated_at);
        let age_days = days_since(&r.updated_at);
        let freshness = RecallFreshness { stale, age_days };
        let rank = facts.len() + 1;
        let admission_reason = if stale {
            "admitted_stale_deprioritized"
        } else {
            "admitted_ranked"
        };
        for signal in &entry.ranking_signals {
            top_level_ranking_signals.push(signal.clone());
        }
        facts.push(RecallFact {
            id: r.id.clone(),
            record_type: r.record_type.as_str().to_string(),
            scope: r.scope.as_str().to_string(),
            content: r.content.clone(),
            confidence: r.confidence,
            repo_id: r.repo_id.clone(),
            related_files: r.related_files.clone(),
            updated_at: r.updated_at.clone(),
            stale,
            policy: RecallFactPolicy {
                rank,
                freshness,
                provenance: RecallProvenance {
                    profile_id: r.profile_id.clone(),
                    workspace_id: r.workspace_id.clone(),
                    repo_id: r.repo_id.clone(),
                    evidence_refs: r.source_ids.clone(),
                    subject_id: r.subject_id.clone(),
                    episode_id: r.episode_id.clone(),
                },
                admission: RecallAdmission {
                    decision: "admitted".to_string(),
                    reason: admission_reason.to_string(),
                    gates: admission_gates.clone(),
                },
                ranking_signals: entry.ranking_signals.clone(),
            },
        });
        citations.push(Citation {
            memory_id: r.id.clone(),
            source_id: r.source_ids.first().cloned(),
            source_path: r
                .metadata
                .get("local_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });
        touched.push(r.id.clone());
        if facts.len() >= 40 {
            truncated = truncated || entry.score > 0.0;
            pack_withheld = scored.len().saturating_sub(facts.len());
            break;
        }
    }
    if params.repo.is_some() {
        top_level_ranking_signals.push("repo_match".to_string());
    }
    if !params.files.is_empty() {
        top_level_ranking_signals.push("file_match".to_string());
    }
    if !params.query.trim().is_empty() {
        top_level_ranking_signals.push("query_match".to_string());
    }
    top_level_ranking_signals.push(format!("pack_mode:{}", params.pack_mode));
    if scored.len() > facts.len() {
        truncated = true;
        if pack_withheld == 0 {
            pack_withheld = scored.len().saturating_sub(facts.len());
        }
    }
    let top_level_ranking_signals = dedupe_ordered_signals(top_level_ranking_signals);
    let withheld = build_withheld(
        recall_omissions.archived,
        recall_omissions.secret_blocked,
        type_filtered,
        pack_withheld,
    );

    // Recent checkpoints (repo-scoped first).
    let checkpoints = store
        .recent_checkpoints(params.profile.as_str(), params.workspace, repo_id, 5)?
        .into_iter()
        .map(|cp| RecallCheckpoint {
            id: cp.id,
            summary: cp.summary,
            branch: cp.branch,
            commit: cp.commit,
            next_steps: cp.next_steps,
            created_at: cp.created_at,
        })
        .collect::<Vec<_>>();

    // Build a short grouped summary string.
    let summary = build_summary(&facts);

    // Touch recalled records' last_used_at (best-effort).
    let _ = store.touch_records(&touched);

    Ok(RecallResponse {
        summary,
        facts,
        checkpoints,
        citations,
        withheld,
        truncated,
        authority: "recall_not_authority".to_string(),
        policy: RecallPolicy {
            authority: "recall_not_authority".to_string(),
            admission_gates,
            ranking_signals: top_level_ranking_signals,
        },
        pack: RecallPack {
            mode: params.pack_mode.to_string(),
            max_tokens: params.max_tokens,
            truncated,
        },
    })
}

fn type_allowed(t: RecordType, include: &[RecordType], exclude: &[RecordType]) -> bool {
    if exclude.contains(&t) {
        return false;
    }
    if !include.is_empty() && !include.contains(&t) {
        return false;
    }
    true
}

/// Score a record for recall (SPEC §8.3 ranking priorities).
fn score_record(
    record: &MemoryRecord,
    repo_id: Option<&str>,
    files: &[String],
    query: &str,
    pack_mode: &str,
) -> f64 {
    let mut score = 0.0;

    // Same profile/workspace is already guaranteed by the filter; give a base.
    score += 1.0;

    // Same repo match (priority 2).
    if let (Some(want), Some(have)) = (repo_id, record.repo_id.as_deref()) {
        if want == have {
            score += 3.0;
        }
    }

    // Exact related-file match (priority 3) — strongest specific signal.
    if !files.is_empty() && !record.related_files.is_empty() {
        let file_match = record.related_files.iter().any(|rf| {
            files
                .iter()
                .any(|f| f == rf || f.ends_with(rf) || rf.ends_with(f.as_str()))
        });
        if file_match {
            score += 4.0;
        }
    }

    // Type weight (priority 4: high-confidence decisions/gotchas/commands).
    score += record.record_type.recall_weight() * 1.5;

    // Confidence.
    score += record.confidence;

    // Recency boost (priority 5/7: recent > old).
    score += recency_boost(&record.updated_at);

    // Lexical overlap with the query terms (helps the LIKE/FTS fallback path).
    score += lexical_overlap(query, &record.content) * 1.5;

    // Checkpoints (priority 5) get a small nudge.
    if matches!(record.record_type, RecordType::TaskCheckpoint) {
        score += 0.5;
    }

    score += pack_mode_boost(record, pack_mode);

    score
}

fn ranking_signals(
    record: &MemoryRecord,
    repo_id: Option<&str>,
    files: &[String],
    query: &str,
    pack_mode: &str,
) -> Vec<String> {
    let mut signals = Vec::new();

    if let (Some(want), Some(have)) = (repo_id, record.repo_id.as_deref()) {
        if want == have {
            signals.push("repo_match".to_string());
        }
    }

    if !files.is_empty() && !record.related_files.is_empty() {
        let file_match = record.related_files.iter().any(|rf| {
            files
                .iter()
                .any(|f| f == rf || f.ends_with(rf) || rf.ends_with(f.as_str()))
        });
        if file_match {
            signals.push("file_match".to_string());
        }
    }

    // recency always contributes to ranking so expose that signal.
    if recency_boost(&record.updated_at) > 0.0 {
        signals.push("recency".to_string());
    }

    if lexical_overlap(query, &record.content) > 0.0 {
        signals.push("query_match".to_string());
    }

    if matches!(record.record_type, RecordType::TaskCheckpoint) {
        signals.push("checkpoint_boost".to_string());
    }

    if record.record_type != RecordType::Other {
        signals.push("record_type_weight".to_string());
    }

    if recency_boost(&record.updated_at) > 0.0 && query.is_empty() {
        signals.push("recent_implicit".to_string());
    }

    if is_stale(&record.updated_at) {
        signals.push("stale_deprioritized".to_string());
    }

    signals.extend(pack_mode_signals(record, pack_mode));

    signals
}

fn pack_mode_boost(record: &MemoryRecord, pack_mode: &str) -> f64 {
    if pack_mode != "debugging" {
        return 0.0;
    }
    let mut boost = match record.record_type {
        RecordType::Gotcha => 3.0,
        RecordType::TaskCheckpoint => 1.5,
        RecordType::Command => 1.0,
        RecordType::WorkflowPattern => 0.75,
        _ => 0.0,
    };
    let content = record.content.to_ascii_lowercase();
    if [
        "debug", "failure", "failed", "error", "rollback", "recover", "gotcha",
    ]
    .iter()
    .any(|needle| content.contains(needle))
    {
        boost += 0.75;
    }
    boost
}

fn pack_mode_signals(record: &MemoryRecord, pack_mode: &str) -> Vec<String> {
    if pack_mode != "debugging" {
        return vec!["pack_mode:default".to_string()];
    }
    let mut signals = vec!["pack_mode:debugging".to_string()];
    match record.record_type {
        RecordType::Gotcha => signals.push("debugging_gotcha".to_string()),
        RecordType::TaskCheckpoint => signals.push("debugging_checkpoint".to_string()),
        RecordType::Command => signals.push("debugging_command".to_string()),
        RecordType::WorkflowPattern => signals.push("debugging_workflow_pattern".to_string()),
        _ => {}
    }
    let content = record.content.to_ascii_lowercase();
    if [
        "debug", "failure", "failed", "error", "rollback", "recover", "gotcha",
    ]
    .iter()
    .any(|needle| content.contains(needle))
    {
        signals.push("debugging_terms".to_string());
    }
    signals
}

fn lexical_overlap(query: &str, content: &str) -> f64 {
    let q_terms: Vec<String> = query
        .to_ascii_lowercase()
        .split_whitespace()
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect();
    if q_terms.is_empty() {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    let hits = q_terms
        .iter()
        .filter(|t| lower.contains(t.as_str()))
        .count();
    hits as f64 / q_terms.len() as f64
}

fn recency_boost(updated_at: &str) -> f64 {
    match days_since(updated_at) {
        Some(days) if days <= 7 => 1.5,
        Some(days) if days <= 30 => 1.0,
        Some(days) if days <= 90 => 0.5,
        Some(_) => 0.1,
        None => 0.3,
    }
}

fn days_since(timestamp: &str) -> Option<i64> {
    let parsed = OffsetDateTime::parse(timestamp, &Rfc3339).ok()?;
    let now = OffsetDateTime::now_utc();
    Some((now - parsed).whole_days())
}

fn is_stale(updated_at: &str) -> bool {
    days_since(updated_at)
        .map(|d| d > STALE_DAYS)
        .unwrap_or(false)
}

fn rfc3339_cutoff(days: i64) -> Option<String> {
    if days <= 0 {
        return None;
    }
    let cutoff = OffsetDateTime::now_utc() - Duration::days(days);
    cutoff.format(&Rfc3339).ok()
}

/// Build a compact grouped summary from the packed facts (SPEC §8.2).
fn build_summary(facts: &[RecallFact]) -> Option<String> {
    if facts.is_empty() {
        return None;
    }
    let count = facts.len();
    let mut decisions = 0;
    let mut gotchas = 0;
    let mut commands = 0;
    let mut prefs = 0;
    for f in facts {
        match f.record_type.as_str() {
            "decision" => decisions += 1,
            "gotcha" => gotchas += 1,
            "command" => commands += 1,
            "preference" => prefs += 1,
            _ => {}
        }
    }
    let mut parts = vec![format!("{count} relevant memory record(s)")];
    if decisions > 0 {
        parts.push(format!("{decisions} decision(s)"));
    }
    if gotchas > 0 {
        parts.push(format!("{gotchas} gotcha(s)"));
    }
    if commands > 0 {
        parts.push(format!("{commands} command(s)"));
    }
    if prefs > 0 {
        parts.push(format!("{prefs} preference(s)"));
    }
    Some(format!(
        "{}. Treat as contextual recall, not authority.",
        parts.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// Explicit search (SPEC §6.3)
// ---------------------------------------------------------------------------

pub struct SearchParams<'a> {
    pub profile: Profile,
    pub workspace: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    pub query: &'a str,
    pub scope: Option<crate::domain::Scope>,
    pub record_type: Option<RecordType>,
    pub include_archived: bool,
    pub limit: usize,
    pub offset: usize,
}

pub fn search(store: &Store, params: &SearchParams) -> Result<SearchResponse> {
    let filters = RecordQuery {
        profile_id: Some(params.profile.as_str().to_string()),
        workspace_id: params.workspace.map(|s| s.to_string()),
        repo_id: params.repo_id.map(|s| s.to_string()),
        record_type: params.record_type,
        scope: params.scope,
        include_archived: params.include_archived,
        recency_cutoff: None,
        // Fetch one extra to detect "there's another page".
        limit: params.limit + 1,
        offset: params.offset,
    };

    let mut records = store.search_records(params.query, &filters)?;
    let has_more = records.len() > params.limit;
    if has_more {
        records.truncate(params.limit);
    }
    let next_cursor = if has_more {
        Some((params.offset + params.limit).to_string())
    } else {
        None
    };

    let matches = records
        .into_iter()
        .map(|r| SearchMatch {
            id: r.id,
            record_type: r.record_type.as_str().to_string(),
            scope: r.scope.as_str().to_string(),
            content: r.content,
            confidence: r.confidence,
            workspace_id: r.workspace_id,
            repo_id: r.repo_id,
            tags: r.tags,
            archived: r.archived,
            updated_at: r.updated_at,
        })
        .collect();

    Ok(SearchResponse {
        matches,
        next_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Portability;
    use crate::domain::Scope;
    use crate::domain::Sensitivity;
    use crate::ids;
    use crate::store::NewRecord;

    fn store_with(records: &[(&str, RecordType, Option<&str>, Vec<&str>)]) -> Store {
        let s = Store::open(":memory:").unwrap();
        s.ensure_workspace("personal", "ws").unwrap();
        for (content, rt, repo, files) in records {
            let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
            let rec = NewRecord {
                profile_id: "personal".to_string(),
                workspace_id: "ws".to_string(),
                repo_id: repo.map(|s| s.to_string()),
                subject_id: None,
                episode_id: None,
                scope: Scope::Repo,
                record_type: *rt,
                content: content.to_string(),
                related_files: files,
                tags: vec![],
                sensitivity: Sensitivity::Personal,
                portability: Portability::ProfileOnly,
                confidence: 0.8,
                source_ids: vec![],
                content_hash: ids::content_hash(
                    "personal",
                    "ws",
                    *repo,
                    rt.as_str(),
                    "repo",
                    content,
                ),
                supersedes: vec![],
                metadata: serde_json::Value::Null,
            };
            s.upsert_record(&rec).unwrap();
        }
        s
    }

    #[test]
    fn recall_prioritizes_repo_and_file_matches() {
        let s = store_with(&[
            (
                "Use axum for HTTP server",
                RecordType::Decision,
                Some("git:repoA"),
                vec!["server.rs"],
            ),
            ("Generic note", RecordType::Other, None, vec![]),
        ]);
        let repo = RepoIdentity {
            repo_id: "git:repoA".to_string(),
            ..Default::default()
        };
        let files = vec!["server.rs".to_string()];
        let params = RecallParams {
            profile: Profile::Personal,
            workspace: "ws",
            repo: Some(&repo),
            query: "http server",
            files: &files,
            max_tokens: 1000,
            pack_mode: "default",
            include_types: &[],
            exclude_types: &[],
            recency_days: None,
        };
        let resp = recall(&s, &params).unwrap();
        assert!(!resp.facts.is_empty());
        assert!(
            resp.facts[0].content.contains("axum"),
            "repo+file match should rank first"
        );
        assert_eq!(resp.authority, "recall_not_authority");
    }

    #[test]
    fn recall_filters_by_profile_workspace() {
        let s = store_with(&[("ws note", RecordType::Decision, None, vec![])]);
        // Different workspace yields nothing.
        let params = RecallParams {
            profile: Profile::Personal,
            workspace: "other-ws",
            repo: None,
            query: "",
            files: &[],
            max_tokens: 1000,
            pack_mode: "default",
            include_types: &[],
            exclude_types: &[],
            recency_days: None,
        };
        let resp = recall(&s, &params).unwrap();
        assert!(resp.facts.is_empty(), "must not leak across workspaces");
    }

    #[test]
    fn search_respects_type_filter() {
        let s = store_with(&[
            ("a decision", RecordType::Decision, None, vec![]),
            ("a command", RecordType::Command, None, vec![]),
        ]);
        let params = SearchParams {
            profile: Profile::Personal,
            workspace: Some("ws"),
            repo_id: None,
            query: "",
            scope: None,
            record_type: Some(RecordType::Command),
            include_archived: false,
            limit: 10,
            offset: 0,
        };
        let resp = search(&s, &params).unwrap();
        assert_eq!(resp.matches.len(), 1);
        assert_eq!(resp.matches[0].record_type, "command");
    }
}
