//! Explicit semantic alias/relation import.
//!
//! This module validates reviewed JSON import files and applies them through
//! the same store methods used by lower-level semantic tests. It never derives
//! relations from memory text and never mutates state during preview.

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

use crate::domain::Relation;
use crate::domain::SubjectAlias;
use crate::error::Error;
use crate::error::Result;
use crate::ids;
use crate::store::Store;

const RELATION_TYPES: &[&str] = &[
    "uses",
    "owns",
    "prefers",
    "works_on",
    "depends_on",
    "supersedes",
    "blocked_by",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticImportRequest {
    pub profile_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub aliases: Vec<SemanticAliasInput>,
    #[serde(default)]
    pub relations: Vec<SemanticRelationInput>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticAliasInput {
    pub subject_id: String,
    pub alias_key: String,
    pub source_evidence: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRelationInput {
    pub from_subject_id: String,
    pub relation_type: String,
    pub to_subject_id: String,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default)]
    pub source_episode_ids: Vec<String>,
    pub source_evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportReport {
    pub profile_id: String,
    pub workspace_id: String,
    pub would_apply: Vec<SemanticImportReportEntry>,
    pub applied: Vec<SemanticImportReportEntry>,
    pub already_present: Vec<SemanticImportReportEntry>,
    pub rejected: Vec<SemanticImportReportEntry>,
    pub counts: SemanticImportCounts,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportReportEntry {
    pub kind: String,
    pub key: String,
    pub status: String,
    pub reason: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticImportCounts {
    pub would_apply: usize,
    pub applied_aliases: usize,
    pub applied_relations: usize,
    pub already_present: usize,
    pub rejected: usize,
}

pub fn preview_semantic_import(
    store: &Store,
    request: &SemanticImportRequest,
) -> Result<SemanticImportReport> {
    validate_request_shape(request)?;
    let mut report = empty_report(request);
    validate_aliases(store, request, &mut report)?;
    validate_relations(store, request, &mut report)?;
    refresh_counts(&mut report);
    Ok(report)
}

pub fn apply_semantic_import(
    store: &Store,
    request: &SemanticImportRequest,
) -> Result<SemanticImportReport> {
    let preview = preview_semantic_import(store, request)?;
    let mut report = empty_report(request);
    report.already_present = preview.already_present;
    report.rejected = preview.rejected;

    for entry in preview.would_apply {
        match entry.kind.as_str() {
            "alias" => {
                let alias: SubjectAlias = serde_json::from_value(entry.value.clone())?;
                let (stored, inserted) = store.insert_or_get_subject_alias(&alias)?;
                let applied = report_entry_for_alias(
                    &stored,
                    if inserted {
                        "applied"
                    } else {
                        "already_present"
                    },
                    None,
                );
                if inserted {
                    report.applied.push(applied);
                } else {
                    report.already_present.push(applied);
                }
            }
            "relation" => {
                let relation: Relation = serde_json::from_value(entry.value.clone())?;
                let (stored, inserted) = store.insert_or_get_relation(&relation)?;
                let applied = report_entry_for_relation(
                    &stored,
                    if inserted {
                        "applied"
                    } else {
                        "already_present"
                    },
                    None,
                );
                if inserted {
                    report.applied.push(applied);
                } else {
                    report.already_present.push(applied);
                }
            }
            _ => {}
        }
    }

    refresh_counts(&mut report);
    Ok(report)
}

fn default_confidence() -> f64 {
    1.0
}

fn validate_request_shape(request: &SemanticImportRequest) -> Result<()> {
    if request.profile_id.trim().is_empty() {
        return Err(Error::invalid_request("missing_profile"));
    }
    if request.workspace_id.trim().is_empty() {
        return Err(Error::invalid_request("missing_workspace"));
    }
    if request.aliases.is_empty() && request.relations.is_empty() {
        return Err(Error::invalid_request("missing_items"));
    }
    Ok(())
}

fn validate_aliases(
    store: &Store,
    request: &SemanticImportRequest,
    report: &mut SemanticImportReport,
) -> Result<()> {
    for alias in &request.aliases {
        let alias_key = alias.alias_key.trim();
        let mut rejected = false;
        if alias.subject_id.trim().is_empty() {
            push_rejected_alias(report, alias, "missing_alias_subject");
            rejected = true;
        }
        if alias_key.is_empty() {
            push_rejected_alias(report, alias, "missing_alias_key");
            rejected = true;
        }
        if alias.source_evidence.trim().is_empty() {
            push_rejected_alias(report, alias, "missing_alias_evidence");
            rejected = true;
        }
        if rejected {
            continue;
        }

        if !store.subject_exists_in_scope(
            &request.profile_id,
            &request.workspace_id,
            alias.subject_id.trim(),
        )? {
            push_rejected_alias(report, alias, "alias_subject_out_of_scope");
            continue;
        }

        let subject_id = alias.subject_id.trim();
        if let Some(existing) =
            store.get_subject_alias(&request.profile_id, &request.workspace_id, alias_key)?
        {
            if existing.subject_id == subject_id {
                report.already_present.push(report_entry_for_alias(
                    &existing,
                    "already_present",
                    None,
                ));
            } else {
                let value = json!({
                    "subject_id": alias.subject_id,
                    "alias_key": alias.alias_key,
                    "source_evidence": alias.source_evidence,
                    "existing_subject_id": existing.subject_id,
                });
                report.rejected.push(report_entry(
                    "alias",
                    alias_key,
                    "rejected",
                    Some("alias_conflict"),
                    value,
                ));
            }
            continue;
        }

        let candidate = SubjectAlias {
            id: ids::new_id("alias"),
            profile_id: request.profile_id.clone(),
            workspace_id: request.workspace_id.clone(),
            subject_id: subject_id.to_string(),
            alias_key: alias_key.to_string(),
            source_evidence: alias.source_evidence.trim().to_string(),
            created_at: ids::now_rfc3339(),
            metadata: json!({"origin": "semantic_import"}),
        };
        report
            .would_apply
            .push(report_entry_for_alias(&candidate, "would_apply", None));
    }
    Ok(())
}

fn validate_relations(
    store: &Store,
    request: &SemanticImportRequest,
    report: &mut SemanticImportReport,
) -> Result<()> {
    for relation in &request.relations {
        let relation_type = relation.relation_type.trim();
        let mut rejected = false;
        if relation.from_subject_id.trim().is_empty() {
            push_rejected_relation(report, relation, "missing_relation_from_subject");
            rejected = true;
        }
        if relation.to_subject_id.trim().is_empty() {
            push_rejected_relation(report, relation, "missing_relation_to_subject");
            rejected = true;
        }
        if relation_type.is_empty() {
            push_rejected_relation(report, relation, "missing_relation_type");
            rejected = true;
        } else if !RELATION_TYPES.contains(&relation_type) {
            push_rejected_relation(report, relation, "unknown_relation_type");
            rejected = true;
        }
        if !(0.0..=1.0).contains(&relation.confidence) || !relation.confidence.is_finite() {
            push_rejected_relation(report, relation, "invalid_relation_confidence");
            rejected = true;
        }
        if relation.source_episode_ids.is_empty()
            || relation
                .source_episode_ids
                .iter()
                .all(|id| id.trim().is_empty())
        {
            push_rejected_relation(report, relation, "missing_relation_episode_evidence");
            rejected = true;
        }
        if relation
            .source_evidence
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            push_rejected_relation(report, relation, "missing_relation_source_evidence");
            rejected = true;
        }
        if rejected {
            continue;
        }

        let from_subject_id = relation.from_subject_id.trim();
        let to_subject_id = relation.to_subject_id.trim();
        let from_in_scope = store.subject_exists_in_scope(
            &request.profile_id,
            &request.workspace_id,
            from_subject_id,
        )?;
        let to_in_scope = store.subject_exists_in_scope(
            &request.profile_id,
            &request.workspace_id,
            to_subject_id,
        )?;
        if !from_in_scope || !to_in_scope {
            push_rejected_relation(report, relation, "relation_endpoint_out_of_scope");
            continue;
        }

        if let Some(existing) = store.get_active_relation(
            &request.profile_id,
            &request.workspace_id,
            from_subject_id,
            relation_type,
            to_subject_id,
        )? {
            report.already_present.push(report_entry_for_relation(
                &existing,
                "already_present",
                None,
            ));
            continue;
        }

        let candidate = Relation {
            id: ids::new_id("relation"),
            profile_id: request.profile_id.clone(),
            workspace_id: request.workspace_id.clone(),
            from_subject_id: from_subject_id.to_string(),
            relation_type: relation_type.to_string(),
            to_subject_id: to_subject_id.to_string(),
            confidence: relation.confidence,
            state: "active".to_string(),
            source_episode_ids: relation
                .source_episode_ids
                .iter()
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect(),
            source_evidence: relation
                .source_evidence
                .as_deref()
                .map(str::trim)
                .filter(|evidence| !evidence.is_empty())
                .map(str::to_string),
            created_at: ids::now_rfc3339(),
            retired_at: None,
            metadata: json!({"origin": "semantic_import"}),
        };
        report
            .would_apply
            .push(report_entry_for_relation(&candidate, "would_apply", None));
    }
    Ok(())
}

fn empty_report(request: &SemanticImportRequest) -> SemanticImportReport {
    SemanticImportReport {
        profile_id: request.profile_id.clone(),
        workspace_id: request.workspace_id.clone(),
        would_apply: Vec::new(),
        applied: Vec::new(),
        already_present: Vec::new(),
        rejected: Vec::new(),
        counts: SemanticImportCounts {
            would_apply: 0,
            applied_aliases: 0,
            applied_relations: 0,
            already_present: 0,
            rejected: 0,
        },
    }
}

fn refresh_counts(report: &mut SemanticImportReport) {
    report.counts.would_apply = report.would_apply.len();
    report.counts.applied_aliases = report
        .applied
        .iter()
        .filter(|entry| entry.kind == "alias")
        .count();
    report.counts.applied_relations = report
        .applied
        .iter()
        .filter(|entry| entry.kind == "relation")
        .count();
    report.counts.already_present = report.already_present.len();
    report.counts.rejected = report.rejected.len();
}

fn push_rejected_alias(
    report: &mut SemanticImportReport,
    alias: &SemanticAliasInput,
    reason: &'static str,
) {
    let key = if alias.alias_key.trim().is_empty() {
        alias.subject_id.as_str()
    } else {
        alias.alias_key.as_str()
    };
    report.rejected.push(report_entry(
        "alias",
        key,
        "rejected",
        Some(reason),
        json!({
            "subject_id": alias.subject_id,
            "alias_key": alias.alias_key,
            "source_evidence": alias.source_evidence,
        }),
    ));
}

fn push_rejected_relation(
    report: &mut SemanticImportReport,
    relation: &SemanticRelationInput,
    reason: &'static str,
) {
    report.rejected.push(report_entry(
        "relation",
        relation_key(
            &relation.from_subject_id,
            &relation.relation_type,
            &relation.to_subject_id,
        ),
        "rejected",
        Some(reason),
        json!({
            "from_subject_id": relation.from_subject_id,
            "relation_type": relation.relation_type,
            "to_subject_id": relation.to_subject_id,
            "confidence": relation.confidence,
            "source_episode_ids": relation.source_episode_ids,
            "source_evidence": relation.source_evidence,
        }),
    ));
}

fn report_entry_for_alias(
    alias: &SubjectAlias,
    status: &str,
    reason: Option<&'static str>,
) -> SemanticImportReportEntry {
    report_entry(
        "alias",
        &alias.alias_key,
        status,
        reason,
        serde_json::to_value(alias).expect("SubjectAlias serializes"),
    )
}

fn report_entry_for_relation(
    relation: &Relation,
    status: &str,
    reason: Option<&'static str>,
) -> SemanticImportReportEntry {
    report_entry(
        "relation",
        relation_key(
            &relation.from_subject_id,
            &relation.relation_type,
            &relation.to_subject_id,
        ),
        status,
        reason,
        serde_json::to_value(relation).expect("Relation serializes"),
    )
}

fn report_entry(
    kind: &str,
    key: impl Into<String>,
    status: &str,
    reason: Option<&'static str>,
    value: Value,
) -> SemanticImportReportEntry {
    SemanticImportReportEntry {
        kind: kind.to_string(),
        key: key.into(),
        status: status.to_string(),
        reason: reason.map(str::to_string),
        value,
    }
}

fn relation_key(from_subject_id: &str, relation_type: &str, to_subject_id: &str) -> String {
    format!("{from_subject_id}:{relation_type}:{to_subject_id}")
}
