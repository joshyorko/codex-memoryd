//! Core domain model (SPEC §4). These are the durable, storage-facing types.
//! The wire/protocol types in [`crate::protocol`] are deliberately separate so
//! the HTTP contract can evolve independently of storage.

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;

/// Required profiles (SPEC §4.1.1). A profile is the top-level portability
/// boundary and MUST be present on every memory record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Personal,
    Work,
    Oss,
    Homelab,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Personal => "personal",
            Profile::Work => "work",
            Profile::Oss => "oss",
            Profile::Homelab => "homelab",
        }
    }

    /// Parse a profile id. Unknown values are rejected by the caller.
    pub fn parse(value: &str) -> Option<Profile> {
        match value.trim().to_ascii_lowercase().as_str() {
            "personal" => Some(Profile::Personal),
            "work" => Some(Profile::Work),
            "oss" => Some(Profile::Oss),
            "homelab" => Some(Profile::Homelab),
            _ => None,
        }
    }

    pub fn all() -> [Profile; 4] {
        [
            Profile::Personal,
            Profile::Work,
            Profile::Oss,
            Profile::Homelab,
        ]
    }
}

/// Memory record scope (SPEC §4.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    Profile,
    Workspace,
    Repo,
    File,
    Session,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Profile => "profile",
            Scope::Workspace => "workspace",
            Scope::Repo => "repo",
            Scope::File => "file",
            Scope::Session => "session",
        }
    }

    pub fn parse(value: &str) -> Option<Scope> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user" => Some(Scope::User),
            "profile" => Some(Scope::Profile),
            "workspace" => Some(Scope::Workspace),
            "repo" => Some(Scope::Repo),
            "file" => Some(Scope::File),
            "session" => Some(Scope::Session),
            _ => None,
        }
    }
}

/// Memory record type (SPEC §4.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    Preference,
    RepoConvention,
    Command,
    Decision,
    Gotcha,
    Landmark,
    TaskCheckpoint,
    Identity,
    WorkflowPattern,
    Other,
}

impl RecordType {
    pub fn as_str(self) -> &'static str {
        match self {
            RecordType::Preference => "preference",
            RecordType::RepoConvention => "repo_convention",
            RecordType::Command => "command",
            RecordType::Decision => "decision",
            RecordType::Gotcha => "gotcha",
            RecordType::Landmark => "landmark",
            RecordType::TaskCheckpoint => "task_checkpoint",
            RecordType::Identity => "identity",
            RecordType::WorkflowPattern => "workflow_pattern",
            RecordType::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<RecordType> {
        match value.trim().to_ascii_lowercase().as_str() {
            "preference" => Some(RecordType::Preference),
            "repo_convention" => Some(RecordType::RepoConvention),
            "command" => Some(RecordType::Command),
            "decision" => Some(RecordType::Decision),
            "gotcha" => Some(RecordType::Gotcha),
            "landmark" => Some(RecordType::Landmark),
            "task_checkpoint" => Some(RecordType::TaskCheckpoint),
            "identity" => Some(RecordType::Identity),
            "workflow_pattern" => Some(RecordType::WorkflowPattern),
            "other" => Some(RecordType::Other),
            _ => None,
        }
    }

    /// Type weight used by recall ranking (SPEC §8.3). Higher is more salient.
    pub fn recall_weight(self) -> f64 {
        match self {
            RecordType::Decision => 1.0,
            RecordType::Gotcha => 0.95,
            RecordType::Command => 0.9,
            RecordType::RepoConvention => 0.85,
            RecordType::TaskCheckpoint => 0.8,
            RecordType::WorkflowPattern => 0.7,
            RecordType::Preference => 0.65,
            RecordType::Landmark => 0.55,
            RecordType::Identity => 0.5,
            RecordType::Other => 0.3,
        }
    }
}

/// Sensitivity classification (SPEC §4.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    Public,
    Personal,
    WorkConfidential,
    SecretBlocked,
}

impl Sensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Sensitivity::Public => "public",
            Sensitivity::Personal => "personal",
            Sensitivity::WorkConfidential => "work_confidential",
            Sensitivity::SecretBlocked => "secret_blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Sensitivity> {
        match value.trim().to_ascii_lowercase().as_str() {
            "public" => Some(Sensitivity::Public),
            "personal" => Some(Sensitivity::Personal),
            "work_confidential" => Some(Sensitivity::WorkConfidential),
            "secret_blocked" => Some(Sensitivity::SecretBlocked),
            _ => None,
        }
    }
}

/// Portability classification (SPEC §4.1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Portability {
    Portable,
    ProfileOnly,
    WorkspaceOnly,
    NeverExport,
}

impl Portability {
    pub fn as_str(self) -> &'static str {
        match self {
            Portability::Portable => "portable",
            Portability::ProfileOnly => "profile_only",
            Portability::WorkspaceOnly => "workspace_only",
            Portability::NeverExport => "never_export",
        }
    }

    pub fn parse(value: &str) -> Option<Portability> {
        match value.trim().to_ascii_lowercase().as_str() {
            "portable" => Some(Portability::Portable),
            "profile_only" => Some(Portability::ProfileOnly),
            "workspace_only" => Some(Portability::WorkspaceOnly),
            "never_export" => Some(Portability::NeverExport),
            _ => None,
        }
    }
}

/// Normalized repository identity (SPEC §4.1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RepoIdentity {
    pub repo_id: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default)]
    pub is_git: bool,
}

/// Durable subject identity for grouping evidence and memories around stable
/// people, repos, projects, workflows, or concepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Person,
    Agent,
    Org,
    Project,
    Repo,
    Routine,
    Workflow,
    Device,
    Concept,
    Other,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SubjectKind::Person => "person",
            SubjectKind::Agent => "agent",
            SubjectKind::Org => "org",
            SubjectKind::Project => "project",
            SubjectKind::Repo => "repo",
            SubjectKind::Routine => "routine",
            SubjectKind::Workflow => "workflow",
            SubjectKind::Device => "device",
            SubjectKind::Concept => "concept",
            SubjectKind::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<SubjectKind> {
        match value.trim().to_ascii_lowercase().as_str() {
            "person" => Some(SubjectKind::Person),
            "agent" => Some(SubjectKind::Agent),
            "org" => Some(SubjectKind::Org),
            "project" => Some(SubjectKind::Project),
            "repo" => Some(SubjectKind::Repo),
            "routine" => Some(SubjectKind::Routine),
            "workflow" => Some(SubjectKind::Workflow),
            "device" => Some(SubjectKind::Device),
            "concept" => Some(SubjectKind::Concept),
            "other" => Some(SubjectKind::Other),
            _ => None,
        }
    }
}

/// Stable entity anchor inside one profile/workspace boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subject {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub subject_key: String,
    pub kind: SubjectKind,
    pub display_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub metadata: Value,
}

/// Append-oriented evidence episode tied to a subject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Episode {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub subject_id: String,
    pub source_kind: String,
    pub source_ref: String,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub status: Option<String>,
    pub summary: String,
    pub trust_level: Option<String>,
    pub source_metadata: Value,
    pub created_at: String,
    pub updated_at: String,
    pub metadata: Value,
}

/// Reviewable reusable procedure derived from repeated successful experience.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Procedure {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub subject_id: Option<String>,
    pub repo_id: Option<String>,
    pub name: String,
    pub activation_query: String,
    pub steps: String,
    pub guardrails: String,
    pub termination_condition: String,
    pub source_episode_ids: Vec<String>,
    pub confidence: f64,
    /// Lifecycle state: candidate | active | retired | superseded | quarantined.
    pub state: String,
    pub created_at: String,
    pub retired_at: Option<String>,
    /// Monotonic version, bumped when a procedure is superseded by a new one.
    pub version: i64,
    /// When this procedure (or its lineage) was first observed.
    pub first_seen: Option<String>,
    /// When the procedure was last validated by successful reuse/eval.
    pub last_validated: Option<String>,
    /// Id of the procedure that supersedes this one (set on the old row).
    pub superseded_by: Option<String>,
    /// Count of failed-reuse / contradiction signals against this procedure.
    pub counter_evidence_count: i64,
    /// Phrases on which this procedure must NOT activate (false-activation guard).
    pub negative_examples: Vec<String>,
    pub metadata: Value,
}

/// The primary durable memory unit (SPEC §4.1.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub subject_id: Option<String>,
    pub episode_id: Option<String>,
    pub scope: Scope,
    #[serde(rename = "type")]
    pub record_type: RecordType,
    pub content: String,
    pub related_files: Vec<String>,
    pub tags: Vec<String>,
    pub sensitivity: Sensitivity,
    pub portability: Portability,
    pub confidence: f64,
    pub source_ids: Vec<String>,
    pub content_hash: String,
    pub supersedes: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
    pub archived: bool,
    #[serde(default = "default_trust_state")]
    pub trust_state: String,
    #[serde(default = "default_trust_score")]
    pub trust_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantine_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quarantined_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_at: Option<String>,
    pub metadata: Value,
}

fn default_trust_state() -> String {
    "trusted".to_string()
}

fn default_trust_score() -> f64 {
    1.0
}

/// An artifact/event a record was derived from (SPEC §4.1.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemorySource {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub kind: String,
    pub source_path: Option<String>,
    pub source_hash: String,
    pub created_at: String,
    pub ingested_at: String,
    pub metadata: Value,
}

/// A resumable summary of project work (SPEC §4.1.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub session_id: Option<String>,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub summary: String,
    pub changed_files: Vec<String>,
    pub decisions: Vec<String>,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
    pub tests_run: Vec<String>,
    pub tests_not_run: Vec<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub created_at: String,
}

/// A durable fact explicitly written by user/agent/ingest (SPEC §4.1.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conclusion {
    pub id: String,
    pub profile_id: String,
    pub workspace_id: String,
    pub repo_id: Option<String>,
    pub target: String,
    pub content: String,
    pub source_id: Option<String>,
    pub created_at: String,
    pub metadata: Value,
}

/// A visible user/assistant message (SPEC §4.1.5). Hidden reasoning is never
/// stored here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisibleTurn {
    pub id: String,
    pub session_id: String,
    pub actor: String,
    pub content: String,
    pub created_at: String,
    pub metadata: Value,
}
