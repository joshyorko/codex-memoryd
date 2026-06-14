//! Procedure activation matching with abstention (issue #145).
//!
//! Procedural memory is dangerous when activation is too broad. The competitive
//! landscape (see `docs/competitive-landscape.md`) shows the field measures
//! procedure *retrieval ranking* but not the **fire-vs-abstain decision**:
//! whether a procedure should activate at all, and whether it correctly stays
//! silent on a similar-but-wrong task. This module makes that decision testable.
//!
//! The matcher is deterministic and content-free in its logic (token overlap),
//! so results are reproducible offline with no model in the loop — the property
//! no hosted competitor can claim.
//!
//! Decision rules, in order:
//! 1. **Negative-example veto.** If the query is strongly similar to any of the
//!    procedure's negative examples, the procedure must NOT activate (abstain).
//! 2. **Positive threshold.** Otherwise the procedure activates only if the
//!    query's token overlap with its activation cues clears a threshold.

use crate::domain::Procedure;

/// Default minimum positive score for a procedure to activate. The positive
/// score is *query coverage*: the fraction of the query's content tokens that
/// appear in the procedure's activation cues. Coverage (not Jaccard) is used so
/// a short, on-point query ("opening a PR") still fires even when the procedure
/// has a large cue set, while an unrelated query covers nothing and abstains.
pub const DEFAULT_ACTIVATION_THRESHOLD: f64 = 0.34;

/// Jaccard similarity at or above which a query is considered to *be* one of the
/// procedure's negative examples, vetoing activation. Jaccard (symmetric) is
/// used here so a correct query that merely shares a token with a negative
/// example is not wrongly vetoed.
pub const NEGATIVE_VETO_THRESHOLD: f64 = 0.5;

/// The outcome of evaluating one procedure against a query.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivationDecision {
    /// Whether the procedure should activate for this query.
    pub activate: bool,
    /// Positive overlap score in [0, 1].
    pub score: f64,
    /// Best similarity to any negative example in [0, 1].
    pub negative_match: f64,
    /// Why the decision was made (stable codes for diagnostics).
    pub reason: &'static str,
}

/// Stopwords stripped before tokenizing so generic activation phrasing
/// ("when working on …") does not inflate overlap.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "to", "of", "and", "or", "on", "in", "for", "with", "when", "working",
    "work", "this", "that", "is", "are", "be", "your", "you", "it", "as", "at", "by", "do",
];

/// Tokenize into lowercase alphanumeric word stems, dropping stopwords and
/// very short tokens.
pub fn tokenize(text: &str) -> Vec<String> {
    text.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 3)
        .filter(|t| !STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn token_set(text: &str) -> std::collections::BTreeSet<String> {
    tokenize(text).into_iter().collect()
}

/// Jaccard similarity between two token sets in [0, 1].
fn jaccard(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Coverage of `query` by `cues`: fraction of query tokens present in cues.
fn coverage(
    query: &std::collections::BTreeSet<String>,
    cues: &std::collections::BTreeSet<String>,
) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let covered = query.iter().filter(|t| cues.contains(*t)).count() as f64;
    covered / query.len() as f64
}

/// The activation cues for a procedure: its activation query, name, and steps.
fn cue_tokens(procedure: &Procedure) -> std::collections::BTreeSet<String> {
    let mut set = token_set(&procedure.activation_query);
    set.extend(token_set(&procedure.name));
    set.extend(token_set(&procedure.steps));
    set
}

/// Decide whether `procedure` should activate for `query`, with abstention on
/// negative examples. Uses the default thresholds.
pub fn evaluate(procedure: &Procedure, query: &str) -> ActivationDecision {
    evaluate_with(
        procedure,
        query,
        DEFAULT_ACTIVATION_THRESHOLD,
        NEGATIVE_VETO_THRESHOLD,
    )
}

/// As [`evaluate`], with explicit thresholds (used by tests and the eval).
pub fn evaluate_with(
    procedure: &Procedure,
    query: &str,
    activation_threshold: f64,
    negative_veto_threshold: f64,
) -> ActivationDecision {
    let query_tokens = token_set(query);
    if query_tokens.is_empty() {
        return ActivationDecision {
            activate: false,
            score: 0.0,
            negative_match: 0.0,
            reason: "empty_query",
        };
    }

    // Rule 1: negative-example veto.
    let mut negative_match = 0.0_f64;
    for negative in &procedure.negative_examples {
        let sim = jaccard(&query_tokens, &token_set(negative));
        if sim > negative_match {
            negative_match = sim;
        }
    }
    if negative_match >= negative_veto_threshold {
        return ActivationDecision {
            activate: false,
            score: 0.0,
            negative_match,
            reason: "negative_example_veto",
        };
    }

    // Rule 2: positive threshold on query coverage.
    let score = coverage(&query_tokens, &cue_tokens(procedure));
    if score >= activation_threshold {
        ActivationDecision {
            activate: true,
            score,
            negative_match,
            reason: "activated",
        }
    } else {
        ActivationDecision {
            activate: false,
            score,
            negative_match,
            reason: "below_threshold",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn proc(activation: &str, name: &str, steps: &str, negatives: &[&str]) -> Procedure {
        Procedure {
            id: "proc_1".to_string(),
            profile_id: "personal".to_string(),
            workspace_id: "default".to_string(),
            subject_id: None,
            repo_id: None,
            name: name.to_string(),
            activation_query: activation.to_string(),
            steps: steps.to_string(),
            guardrails: String::new(),
            termination_condition: String::new(),
            source_episode_ids: vec![],
            confidence: 0.8,
            state: "active".to_string(),
            created_at: "2030-01-01T00:00:00Z".to_string(),
            retired_at: None,
            version: 1,
            first_seen: None,
            last_validated: None,
            superseded_by: None,
            counter_evidence_count: 0,
            negative_examples: negatives.iter().map(|s| s.to_string()).collect(),
            metadata: json!({}),
        }
    }

    #[test]
    fn activates_on_related_query() {
        let p = proc(
            "When opening a pull request",
            "open a pull request",
            "review the diff, run cargo test, write rollback notes",
            &[],
        );
        let d = evaluate(&p, "opening a pull request");
        assert!(d.activate, "related query should activate: {d:?}");
    }

    #[test]
    fn abstains_on_unrelated_query() {
        let p = proc(
            "When opening a pull request",
            "open a pull request",
            "review the diff, run cargo test",
            &[],
        );
        let d = evaluate(&p, "configure the office coffee machine schedule");
        assert!(!d.activate, "unrelated query must abstain: {d:?}");
        assert_eq!(d.reason, "below_threshold");
    }

    #[test]
    fn vetoes_on_negative_example() {
        let p = proc(
            "When deploying to production",
            "deploy to production",
            "run the release pipeline and tag the version",
            &["deploying to a local development sandbox"],
        );
        // Similar-but-wrong task that matches the negative example must abstain.
        let d = evaluate(&p, "deploying to a local development sandbox");
        assert!(!d.activate, "negative example must veto: {d:?}");
        assert_eq!(d.reason, "negative_example_veto");
    }

    #[test]
    fn empty_query_abstains() {
        let p = proc("When X", "x", "do y", &[]);
        let d = evaluate(&p, "   ");
        assert!(!d.activate);
        assert_eq!(d.reason, "empty_query");
    }
}
