//! Actual service capture for positive lifecycle fixtures, without invented
//! speaker labels, source IDs, or provenance upgrades.

use codex_memoryd::domain::{MemoryRecord, RepoIdentity};
use codex_memoryd::protocol::CheckpointRequest;
use codex_memoryd::service::Service;
use codex_memoryd::store::RecordQuery;

pub fn capture(
    service: &Service,
    profile: &str,
    workspace: &str,
    repo: Option<RepoIdentity>,
    summary: &str,
) -> MemoryRecord {
    let response = service
        .checkpoint(CheckpointRequest {
            profile: Some(profile.into()),
            workspace: Some(workspace.into()),
            repo,
            session: None,
            summary: Some(summary.into()),
            changed_files: vec![],
            decisions: vec![],
            blockers: vec![],
            next_steps: vec![],
            tests_run: vec![],
            tests_not_run: vec![],
            branch: None,
            commit: None,
        })
        .expect("capture primary checkpoint");
    service
        .store
        .query_records(&RecordQuery {
            profile_id: Some(profile.into()),
            workspace_id: Some(workspace.into()),
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .find(|record| record.metadata["checkpoint_id"] == response.id)
        .expect("actual captured checkpoint record")
}
