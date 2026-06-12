//! Safe export of memory records (SPEC §6.8). Enforces the profile-boundary
//! matrix, omits `secret_blocked` records, and emits JSONL by default.

use crate::domain::MemoryRecord;
use crate::domain::Profile;
use crate::error::Error;
use crate::error::Result;
use crate::policy;
use crate::policy::BoundaryDecision;
use crate::store::RecordQuery;
use crate::store::Store;

pub struct ExportParams<'a> {
    pub profile: Profile,
    pub workspace: Option<&'a str>,
    pub repo_id: Option<&'a str>,
    pub include_archived: bool,
    pub format: ExportFormat,
    /// Optional destination profile. When set, the boundary matrix applies.
    pub target_profile: Option<Profile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jsonl,
    Json,
}

impl ExportFormat {
    pub fn parse(value: Option<&str>) -> ExportFormat {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("json") => ExportFormat::Json,
            _ => ExportFormat::Jsonl,
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            ExportFormat::Jsonl => "application/x-ndjson",
            ExportFormat::Json => "application/json",
        }
    }
}

/// Result of an export run.
#[derive(Debug)]
pub struct ExportResult {
    pub body: String,
    pub content_type: &'static str,
    pub record_count: usize,
    pub omitted_secret: usize,
    pub omitted_boundary: usize,
}

/// Gather and serialize exportable records, applying boundary + secret rules.
pub fn export(store: &Store, params: &ExportParams) -> Result<ExportResult> {
    // Apply boundary policy up front when a target profile is provided.
    let boundary = match params.target_profile {
        Some(target) => policy::export_boundary(params.profile, target),
        None => BoundaryDecision::Allow,
    };
    if let BoundaryDecision::Deny { reason } = &boundary {
        let _ = store.record_policy_event(
            Some(params.profile.as_str()),
            params.workspace,
            "boundary_denied",
            "profile_boundary_denied",
            reason,
            "export",
        );
        return Err(Error::profile_boundary(reason.clone()));
    }

    let filters = RecordQuery {
        profile_id: Some(params.profile.as_str().to_string()),
        workspace_id: params.workspace.map(|s| s.to_string()),
        repo_id: params.repo_id.map(|s| s.to_string()),
        record_type: None,
        scope: None,
        include_archived: params.include_archived,
        recency_cutoff: None,
        limit: 0, // unlimited for export
        offset: 0,
    };

    let (all, omitted_secret) = store.export_records(&filters)?;
    let mut omitted_boundary = 0usize;
    let mut exported: Vec<MemoryRecord> = Vec::new();

    for record in all {
        // For personal->work, only generic operating preferences cross.
        if matches!(boundary, BoundaryDecision::AllowGenericPreferencesOnly)
            && !policy::is_generic_preference(record.record_type, record.sensitivity)
        {
            omitted_boundary += 1;
            continue;
        }
        // Never export records explicitly marked never_export across profiles.
        if params.target_profile.is_some()
            && matches!(record.portability, crate::domain::Portability::NeverExport)
        {
            omitted_boundary += 1;
            continue;
        }
        exported.push(record);
    }

    let body = match params.format {
        ExportFormat::Jsonl => {
            let mut out = String::new();
            for record in &exported {
                out.push_str(&serde_json::to_string(record)?);
                out.push('\n');
            }
            out
        }
        ExportFormat::Json => serde_json::to_string_pretty(&exported)?,
    };

    Ok(ExportResult {
        body,
        content_type: params.format.content_type(),
        record_count: exported.len(),
        omitted_secret,
        omitted_boundary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Portability;
    use crate::domain::RecordType;
    use crate::domain::Scope;
    use crate::domain::Sensitivity;
    use crate::ids;
    use crate::store::NewRecord;

    fn store_one(profile: &str, sensitivity: Sensitivity, rt: RecordType) -> Store {
        let s = Store::open(":memory:").unwrap();
        s.ensure_workspace(profile, "ws").unwrap();
        let rec = NewRecord {
            profile_id: profile.to_string(),
            workspace_id: "ws".to_string(),
            repo_id: None,
            scope: Scope::Workspace,
            record_type: rt,
            content: "exportable content".to_string(),
            related_files: vec![],
            tags: vec![],
            sensitivity,
            portability: Portability::Portable,
            confidence: 0.8,
            source_ids: vec![],
            content_hash: ids::content_hash(
                profile,
                "ws",
                None,
                rt.as_str(),
                "workspace",
                "exportable content",
            ),
            supersedes: vec![],
            metadata: serde_json::Value::Null,
        };
        s.upsert_record(&rec).unwrap();
        s
    }

    #[test]
    fn work_to_personal_denied() {
        let s = store_one("work", Sensitivity::WorkConfidential, RecordType::Decision);
        let params = ExportParams {
            profile: Profile::Work,
            workspace: None,
            repo_id: None,
            include_archived: false,
            format: ExportFormat::Jsonl,
            target_profile: Some(Profile::Personal),
        };
        let err = export(&s, &params).unwrap_err();
        assert_eq!(err.code, crate::error::ErrorCode::ProfileBoundaryDenied);
    }

    #[test]
    fn export_omits_secret_blocked() {
        let s = store_one("personal", Sensitivity::SecretBlocked, RecordType::Other);
        let params = ExportParams {
            profile: Profile::Personal,
            workspace: None,
            repo_id: None,
            include_archived: false,
            format: ExportFormat::Jsonl,
            target_profile: None,
        };
        let result = export(&s, &params).unwrap();
        assert_eq!(result.record_count, 0);
        assert_eq!(result.omitted_secret, 1);
        assert!(result.body.trim().is_empty());
    }

    #[test]
    fn personal_to_work_only_generic_prefs() {
        let s = store_one("personal", Sensitivity::Personal, RecordType::Decision);
        let params = ExportParams {
            profile: Profile::Personal,
            workspace: None,
            repo_id: None,
            include_archived: false,
            format: ExportFormat::Jsonl,
            target_profile: Some(Profile::Work),
        };
        // A decision is not a generic preference → omitted.
        let result = export(&s, &params).unwrap();
        assert_eq!(result.record_count, 0);
        assert_eq!(result.omitted_boundary, 1);
    }
}
