use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONSOLIDATION_CONTRACT_VERSION: &str = "consolidation.v1";
const MAX_ITEMS: usize = 10_000;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConsolidationMode {
    Off,
    Preview,
    Automatic,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationOperation {
    AdoptStatement,
    AdoptInference,
    Reinforce,
    Supersede,
    NoChange,
    Defer,
    Reject,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidationStatus {
    Proposed,
    Validated,
    Applied,
    Deferred,
    Rejected,
    Conflict,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationBudget {
    pub max_candidates: usize,
    pub max_source_records: usize,
    pub max_provider_calls: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
}

impl ConsolidationBudget {
    pub fn validate(&self) -> Result<(), String> {
        let values = [
            self.max_candidates,
            self.max_source_records,
            self.max_provider_calls,
        ];
        if values.iter().any(|value| *value == 0 || *value > MAX_ITEMS) {
            return Err("budget item limits must be finite and within bounds".into());
        }
        if self.max_input_bytes == 0
            || self.max_output_bytes == 0
            || self.max_input_bytes > MAX_BYTES
            || self.max_output_bytes > MAX_BYTES
        {
            return Err("budget byte limits are invalid".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationPolicy {
    pub contract_version: String,
    pub mode: ConsolidationMode,
    pub scopes: Vec<String>,
    pub claim_classes: Vec<String>,
    pub source_classes: Vec<String>,
    pub operations: Vec<ConsolidationOperation>,
    pub budget: ConsolidationBudget,
    pub retention_days: u32,
    pub semantic_validation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_metadata: Option<Value>,
}

impl ConsolidationPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.contract_version != CONSOLIDATION_CONTRACT_VERSION {
            return Err("unsupported consolidation contract version".into());
        }
        if self.scopes.is_empty() || self.scopes.len() > MAX_ITEMS {
            return Err("policy scopes must be bounded and non-empty".into());
        }
        if self.retention_days == 0 {
            return Err("retention_days must be positive".into());
        }
        self.budget.validate()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationSourceCursor {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    pub explicit_since: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationCandidate {
    pub candidate_id: String,
    pub output_digest: String,
    pub claim: String,
    pub claim_class: String,
    pub subject: String,
    pub inferred: bool,
    pub source_ids: Vec<String>,
    pub supporting_spans: Vec<String>,
}

impl ConsolidationCandidate {
    pub fn validate(&self) -> Result<(), String> {
        if self.candidate_id.is_empty() || self.output_digest.is_empty() || self.claim.is_empty() {
            return Err("candidate identity and claim are required".into());
        }
        if self.source_ids.is_empty()
            || self.source_ids.len() > MAX_ITEMS
            || self.supporting_spans.len() > MAX_ITEMS
        {
            return Err("candidate evidence is missing or unbounded".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationBatch {
    pub contract_version: String,
    pub batch_id: String,
    pub policy_digest: String,
    pub scope: String,
    pub source_cursor: ConsolidationSourceCursor,
    pub snapshot_digest: String,
    pub candidates: Vec<ConsolidationCandidate>,
}

impl ConsolidationBatch {
    pub fn validate(&self) -> Result<(), String> {
        if self.contract_version != CONSOLIDATION_CONTRACT_VERSION
            || self.batch_id.is_empty()
            || self.policy_digest.is_empty()
            || self.snapshot_digest.is_empty()
        {
            return Err("batch identity or contract version is invalid".into());
        }
        if self.candidates.is_empty() || self.candidates.len() > MAX_ITEMS {
            return Err("batch candidates must be bounded and non-empty".into());
        }
        for candidate in &self.candidates {
            candidate.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationDecision {
    pub candidate_id: String,
    pub output_digest: String,
    pub operation: ConsolidationOperation,
    pub reason: String,
    pub distinct_evidence_roots: Vec<String>,
    pub validator: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConsolidationReceipt {
    pub contract_version: String,
    pub batch_id: String,
    pub decision_digest: String,
    pub status: ConsolidationStatus,
    pub applied_record_ids: Vec<String>,
    pub correction_generation: u64,
}
