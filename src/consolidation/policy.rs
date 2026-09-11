use std::collections::BTreeSet;

use super::{
    ConsolidationCandidate, ConsolidationDecision, ConsolidationMode, ConsolidationOperation,
    ConsolidationPolicy,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceDescriptor {
    pub id: String,
    pub root_id: String,
    pub source_class: String,
    pub actor: String,
    pub subject: String,
    pub content: String,
    pub supporting_span: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticValidation {
    pub supported: bool,
    pub validator: String,
    pub reason: String,
}

pub fn evaluate_candidate(
    policy: &ConsolidationPolicy,
    scope: &str,
    candidate: &ConsolidationCandidate,
    evidence: &[EvidenceDescriptor],
    current_claims: &[String],
    current_roots: &[String],
    validation: Option<&SemanticValidation>,
) -> ConsolidationDecision {
    let defer = |reason: &str| ConsolidationDecision {
        candidate_id: candidate.candidate_id.clone(),
        output_digest: candidate.output_digest.clone(),
        operation: ConsolidationOperation::Defer,
        reason: reason.into(),
        distinct_evidence_roots: vec![],
        validator: validation.map(|result| result.validator.clone()),
    };
    if policy.validate().is_err() {
        return defer("invalid_policy");
    }
    if policy.mode == ConsolidationMode::Off {
        return defer("policy_off");
    }
    if !policy.scopes.iter().any(|allowed| allowed == scope) {
        return defer("scope_not_allowed");
    }
    if !policy
        .claim_classes
        .iter()
        .any(|allowed| allowed == &candidate.claim_class)
    {
        return defer("claim_class_not_allowed");
    }
    if candidate.validate().is_err() {
        return defer("malformed_candidate");
    }
    match crate::policy::screen_content(&candidate.claim, crate::policy::MAX_RECORD_CHARS) {
        crate::policy::PolicyDecision::Accept(_) => {}
        crate::policy::PolicyDecision::Reject { .. } => return defer("candidate_content_rejected"),
    }
    if !candidate.inferred {
        let claim = candidate.claim.to_ascii_lowercase();
        if claim.contains("just for this task")
            || claim.contains("only for this task")
            || claim.contains("hypothetically")
            || claim.contains("as an example")
            || claim.contains("quoted")
            || claim.contains("do not remember")
            || claim.contains("don't remember")
            || claim.starts_with("not ")
        {
            return defer("statement_scope_or_negation_uncertain");
        }
    }
    let referenced: BTreeSet<&str> = candidate.source_ids.iter().map(String::as_str).collect();
    let matched: Vec<&EvidenceDescriptor> = evidence
        .iter()
        .filter(|item| referenced.contains(item.id.as_str()))
        .collect();
    let roots: BTreeSet<&str> = matched.iter().map(|item| item.root_id.as_str()).collect();
    if matched.len() != referenced.len() || roots.is_empty() {
        return defer("unresolved_source_reference");
    }
    if matched.iter().any(|item| {
        !policy
            .source_classes
            .iter()
            .any(|allowed| allowed == &item.source_class)
    }) {
        return defer("source_class_not_allowed");
    }
    if matched.iter().any(|item| item.subject != candidate.subject) {
        return defer("subject_scope_mismatch");
    }
    if current_claims.iter().any(|claim| claim == &candidate.claim)
        && roots
            .iter()
            .all(|root| current_roots.iter().any(|known| known == root))
    {
        return ConsolidationDecision {
            candidate_id: candidate.candidate_id.clone(),
            output_digest: candidate.output_digest.clone(),
            operation: ConsolidationOperation::NoChange,
            reason: "claim_and_evidence_already_represented".into(),
            distinct_evidence_roots: roots.iter().map(|root| (*root).into()).collect(),
            validator: validation.map(|result| result.validator.clone()),
        };
    }
    if candidate.inferred {
        if !policy.semantic_validation {
            return defer("semantic_validation_disabled");
        }
        let Some(result) = validation else {
            return defer("semantic_validation_unavailable");
        };
        if !result.supported {
            return defer("semantic_support_not_established");
        }
    }
    let operation = if candidate.inferred {
        ConsolidationOperation::AdoptInference
    } else {
        ConsolidationOperation::AdoptStatement
    };
    ConsolidationDecision {
        candidate_id: candidate.candidate_id.clone(),
        output_digest: candidate.output_digest.clone(),
        operation,
        reason: "bounded_source_and_semantic_checks_passed".into(),
        distinct_evidence_roots: roots.iter().map(|root| (*root).into()).collect(),
        validator: validation.map(|result| result.validator.clone()),
    }
}
