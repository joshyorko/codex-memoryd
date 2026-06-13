//! Local Git history import. This is an evidence-only ingest path: accepted
//! commit trailers become subject episodes, never active memory records.

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use serde_json::json;

use crate::error::Error;
use crate::error::ErrorCode;
use crate::error::Result;
use crate::ids;
use crate::policy;
use crate::policy::PolicyDecision;
use crate::protocol::EpisodeCreateRequest;
use crate::protocol::SubjectCreateRequest;
use crate::service::Service;
use crate::store::ledger_safe_summary;
use crate::store::EvidenceLedgerEntry;

const SUPPORTED_TRAILERS: &[(&str, &str)] = &[
    ("Memory-Decision", "decision"),
    ("Memory-Rejected", "rejected"),
    ("Memory-Verify", "verify"),
    ("Memory-Gotcha", "gotcha"),
    ("Memory-Procedure", "procedure"),
    ("Memory-Scar", "scar"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitImportMode {
    Preview,
    Apply,
}

impl GitImportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GitImportMode::Preview => "preview",
            GitImportMode::Apply => "apply",
        }
    }
}

pub struct GitImportParams<'a> {
    pub repo_path: &'a Path,
    pub profile: Option<String>,
    pub workspace: Option<String>,
    pub mode: GitImportMode,
    pub max_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitImportEpisodePreview {
    pub trailer: String,
    pub source_ref: String,
    pub summary: String,
    pub commit: String,
    pub authored_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitImportRejection {
    pub source_ref: String,
    pub code: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitImportResponse {
    pub mode: String,
    pub repo_path: String,
    pub subject_id: Option<String>,
    pub commits_scanned: usize,
    pub proposed: usize,
    pub created: usize,
    pub skipped: usize,
    pub rejected: usize,
    pub episodes: Vec<GitImportEpisodePreview>,
    pub rejections: Vec<GitImportRejection>,
}

#[derive(Debug, Clone)]
struct CommitEntry {
    sha: String,
    authored_at: Option<String>,
    author_name: Option<String>,
    body: String,
}

pub fn run(service: &Service, params: GitImportParams<'_>) -> Result<GitImportResponse> {
    let repo_root = git_repo_root(params.repo_path)?;
    let commits = read_commits(&repo_root, params.max_count.max(1))?;
    let mut episodes = Vec::new();
    let mut rejections = Vec::new();

    for commit in &commits {
        for (index, (trailer, value)) in parse_memory_trailers(&commit.body).into_iter().enumerate()
        {
            let trailer_kind = trailer_kind(&trailer).unwrap_or("other");
            let source_ref = format!(
                "git:{}:{}:{}",
                commit.sha,
                trailer.to_ascii_lowercase(),
                index
            );
            let summary = format!("{}: {}", trailer_label(trailer_kind), value);
            match policy::screen_string_value(&summary) {
                PolicyDecision::Accept(cleaned) => {
                    episodes.push(GitImportEpisodePreview {
                        trailer,
                        source_ref,
                        summary: cleaned,
                        commit: commit.sha.clone(),
                        authored_at: commit.authored_at.clone(),
                    });
                }
                PolicyDecision::Reject { code, reason } => {
                    rejections.push(GitImportRejection {
                        source_ref,
                        code,
                        reason,
                    });
                }
            }
        }
    }

    let mut subject_id = None;
    let mut created = 0usize;
    let mut skipped = 0usize;

    if params.mode == GitImportMode::Apply && (!episodes.is_empty() || !rejections.is_empty()) {
        let subject = service.create_subject(SubjectCreateRequest {
            profile: params.profile.clone(),
            workspace: params.workspace.clone(),
            subject_key: Some(format!("git:{}", repo_root.display())),
            kind: Some("repo".to_string()),
            display_name: Some(
                repo_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("git repository")
                    .to_string(),
            ),
            metadata: Some(
                json!({"origin": "git-import", "repo_path": repo_root.display().to_string()}),
            ),
        })?;
        subject_id = Some(subject.subject.id.clone());

        for episode in &episodes {
            if service
                .store
                .find_episode_by_source(
                    &subject.subject.profile_id,
                    &subject.subject.workspace_id,
                    "git_commit_trailer",
                    &episode.source_ref,
                )?
                .is_some()
            {
                skipped += 1;
                continue;
            }

            let created_episode = service.create_episode(EpisodeCreateRequest {
                profile: Some(subject.subject.profile_id.clone()),
                workspace: Some(subject.subject.workspace_id.clone()),
                subject_id: Some(subject.subject.id.clone()),
                source_kind: Some("git_commit_trailer".to_string()),
                source_ref: Some(episode.source_ref.clone()),
                started_at: episode.authored_at.clone(),
                ended_at: None,
                status: Some("evidence".to_string()),
                summary: Some(episode.summary.clone()),
                trust_level: Some("medium".to_string()),
                source_metadata: Some(json!({
                    "commit": episode.commit,
                    "trailer": episode.trailer,
                    "author_name": commits
                        .iter()
                        .find(|commit| commit.sha == episode.commit)
                        .and_then(|commit| commit.author_name.clone()),
                })),
                metadata: Some(json!({"origin": "git-import"})),
            })?;
            service.store.record_evidence_ledger(&EvidenceLedgerEntry {
                profile_id: subject.subject.profile_id.clone(),
                workspace_id: subject.subject.workspace_id.clone(),
                repo_id: Some(format!("git:{}", repo_root.display())),
                subject_key: Some(subject.subject.subject_key.clone()),
                source_kind: "git_commit_trailer".to_string(),
                source_id: Some(created_episode.episode.id),
                source_path: Some(episode.source_ref.clone()),
                source_hash: ids::source_hash(
                    &subject.subject.profile_id,
                    &subject.subject.workspace_id,
                    &episode.source_ref,
                    &episode.summary,
                ),
                safe_summary: ledger_safe_summary(&episode.summary),
                policy_state: "accepted".to_string(),
                metadata: json!({
                    "origin": "git-import",
                    "commit": episode.commit,
                    "trailer": episode.trailer,
                }),
            })?;
            created += 1;
        }

        for rejection in &rejections {
            service.store.record_evidence_ledger(&EvidenceLedgerEntry {
                profile_id: subject.subject.profile_id.clone(),
                workspace_id: subject.subject.workspace_id.clone(),
                repo_id: Some(format!("git:{}", repo_root.display())),
                subject_key: Some(subject.subject.subject_key.clone()),
                source_kind: "git_commit_trailer".to_string(),
                source_id: None,
                source_path: Some(rejection.source_ref.clone()),
                source_hash: ids::source_hash(
                    &subject.subject.profile_id,
                    &subject.subject.workspace_id,
                    &rejection.source_ref,
                    &rejection.reason,
                ),
                safe_summary: ledger_safe_summary(&format!(
                    "rejected git import trailer: {}",
                    rejection.reason
                )),
                policy_state: rejection.code.clone(),
                metadata: json!({"origin": "git-import"}),
            })?;
        }
    }

    Ok(GitImportResponse {
        mode: params.mode.as_str().to_string(),
        repo_path: repo_root.display().to_string(),
        subject_id,
        commits_scanned: commits.len(),
        proposed: episodes.len(),
        created,
        skipped,
        rejected: rejections.len(),
        episodes,
        rejections,
    })
}

fn git_repo_root(path: &Path) -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|e| Error::storage(format!("failed to run git rev-parse: {e}")))?;
    if !output.status.success() {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            format!("not a git repository: {}", path.display()),
        ));
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}

fn read_commits(repo_root: &Path, max_count: usize) -> Result<Vec<CommitEntry>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "log",
            "--no-decorate",
            &format!("--max-count={max_count}"),
            "--format=%H%x1f%aI%x1f%an%x1f%B%x1e",
        ])
        .output()
        .map_err(|e| Error::storage(format!("failed to run git log: {e}")))?;
    if !output.status.success() {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            format!("git log failed for {}", repo_root.display()),
        ));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    for record in raw.split('\x1e') {
        let record = record.trim_matches('\n');
        if record.trim().is_empty() {
            continue;
        }
        let mut parts = record.splitn(4, '\x1f');
        let Some(sha) = parts.next() else { continue };
        let authored_at = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let author_name = parts.next().filter(|s| !s.is_empty()).map(str::to_string);
        let body = parts.next().unwrap_or_default().to_string();
        commits.push(CommitEntry {
            sha: sha.to_string(),
            authored_at,
            author_name,
            body,
        });
    }
    Ok(commits)
}

fn parse_memory_trailers(body: &str) -> Vec<(String, String)> {
    let mut trailers = Vec::new();
    for line in body.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if SUPPORTED_TRAILERS
            .iter()
            .any(|(supported, _)| supported.eq_ignore_ascii_case(key))
        {
            let value = value.trim();
            if !value.is_empty() {
                trailers.push((key.to_string(), value.to_string()));
            }
        }
    }
    trailers
}

fn trailer_kind(trailer: &str) -> Option<&'static str> {
    SUPPORTED_TRAILERS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(trailer))
        .map(|(_, kind)| *kind)
}

fn trailer_label(kind: &str) -> &'static str {
    match kind {
        "decision" => "Decision",
        "rejected" => "Rejected",
        "verify" => "Verify",
        "gotcha" => "Gotcha",
        "procedure" => "Procedure",
        "scar" => "Scar",
        _ => "Evidence",
    }
}
