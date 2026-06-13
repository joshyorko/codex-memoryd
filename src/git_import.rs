//! Local Git history import. This is an evidence-only ingest path: accepted
//! commit trailers become subject episodes, never active memory records.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

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
    pub refs_fixture: Option<&'a Path>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    pub authored_at: Option<String>,
    #[serde(skip)]
    source_root: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitImportRejection {
    pub source_ref: String,
    pub code: String,
    pub reason: String,
    #[serde(skip)]
    source_root: String,
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

#[derive(Debug, Clone, Deserialize)]
struct RefsFixtureItem {
    kind: String,
    repo: String,
    #[serde(default)]
    number: Option<Value>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    authored_at: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default, alias = "text")]
    body: Option<String>,
}

#[derive(Debug, Clone)]
struct ImportSource {
    source_kind: &'static str,
    source_ref_root: String,
    commit: Option<String>,
    authored_at: Option<String>,
    body: String,
    source_metadata: Value,
}

pub fn run(service: &Service, params: GitImportParams<'_>) -> Result<GitImportResponse> {
    let repo_root = git_repo_root(params.repo_path)?;
    let sources = if let Some(refs_fixture) = params.refs_fixture {
        read_refs_fixture_sources(&repo_root, refs_fixture, params.max_count.max(1))?
    } else {
        read_commits(&repo_root, params.max_count.max(1))?
            .into_iter()
            .map(|commit| {
                let source_ref_root = commit.sha.clone();
                let authored_at = commit.authored_at.clone();
                let source_metadata = json!({
                    "origin": "git-import",
                    "commit": commit.sha,
                    "author_name": commit.author_name,
                    "authored_at": authored_at.clone(),
                });
                ImportSource {
                    source_kind: "git_commit_trailer",
                    source_ref_root,
                    commit: Some(commit.sha),
                    authored_at,
                    body: commit.body,
                    source_metadata,
                }
            })
            .collect()
    };
    let mut episodes = Vec::new();
    let mut rejections = Vec::new();

    for source in &sources {
        for (index, (trailer, value)) in parse_memory_trailers(&source.body).into_iter().enumerate()
        {
            let trailer_kind = trailer_kind(&trailer).unwrap_or("other");
            let source_ref = source_ref_for_trailer(source, &trailer, index);
            let summary = format!("{}: {}", trailer_label(trailer_kind), value);
            match policy::screen_string_value(&summary) {
                PolicyDecision::Accept(cleaned) => {
                    episodes.push(GitImportEpisodePreview {
                        trailer,
                        source_ref,
                        summary: cleaned,
                        commit: source.commit.clone(),
                        source: if source.source_kind == "git_commit_trailer" {
                            None
                        } else {
                            Some(source.source_metadata.clone())
                        },
                        authored_at: source.authored_at.clone(),
                        source_root: source.source_ref_root.clone(),
                    });
                }
                PolicyDecision::Reject { code, reason } => {
                    rejections.push(GitImportRejection {
                        source_ref,
                        code,
                        reason,
                        source_root: source.source_ref_root.clone(),
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
        let source_by_root: std::collections::HashMap<&str, &ImportSource> = sources
            .iter()
            .map(|source| (source.source_ref_root.as_str(), source))
            .collect();

        for episode in &episodes {
            let Some(source) = source_by_root.get(episode.source_root.as_str()) else {
                return Err(Error::internal(format!(
                    "missing import source for {}",
                    episode.source_root
                )));
            };
            if service
                .store
                .find_episode_by_source(
                    &subject.subject.profile_id,
                    &subject.subject.workspace_id,
                    source.source_kind,
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
                source_kind: Some(source.source_kind.to_string()),
                source_ref: Some(episode.source_ref.clone()),
                started_at: episode.authored_at.clone(),
                ended_at: None,
                status: Some("evidence".to_string()),
                summary: Some(episode.summary.clone()),
                trust_level: Some("medium".to_string()),
                source_metadata: Some(source_metadata_with_trailer(
                    &source.source_metadata,
                    &episode.trailer,
                )),
                metadata: Some(json!({"origin": "git-import"})),
            })?;
            service.store.record_evidence_ledger(&EvidenceLedgerEntry {
                profile_id: subject.subject.profile_id.clone(),
                workspace_id: subject.subject.workspace_id.clone(),
                repo_id: Some(format!("git:{}", repo_root.display())),
                subject_key: Some(subject.subject.subject_key.clone()),
                source_kind: source.source_kind.to_string(),
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
                    "source_kind": source.source_kind,
                    "source_root": source.source_ref_root.clone(),
                    "trailer": episode.trailer,
                    "source": source.source_metadata.clone(),
                }),
            })?;
            created += 1;
        }

        for rejection in &rejections {
            let Some(source) = source_by_root.get(rejection.source_root.as_str()) else {
                return Err(Error::internal(format!(
                    "missing import source for rejection {}",
                    rejection.source_ref
                )));
            };
            service.store.record_evidence_ledger(&EvidenceLedgerEntry {
                profile_id: subject.subject.profile_id.clone(),
                workspace_id: subject.subject.workspace_id.clone(),
                repo_id: Some(format!("git:{}", repo_root.display())),
                subject_key: Some(subject.subject.subject_key.clone()),
                source_kind: source.source_kind.to_string(),
                source_id: None,
                source_path: Some(rejection.source_ref.clone()),
                source_hash: ids::source_hash(
                    &subject.subject.profile_id,
                    &subject.subject.workspace_id,
                    &rejection.source_ref,
                    &rejection.reason,
                ),
                safe_summary: ledger_safe_summary(&format!(
                    "rejected git import source: {}",
                    rejection.reason
                )),
                policy_state: rejection.code.clone(),
                metadata: json!({
                    "origin": "git-import",
                    "source_kind": source.source_kind,
                    "source_root": source.source_ref_root.clone(),
                    "source": source.source_metadata.clone(),
                }),
            })?;
        }
    }

    Ok(GitImportResponse {
        mode: params.mode.as_str().to_string(),
        repo_path: repo_root.display().to_string(),
        subject_id,
        commits_scanned: sources.len(),
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

fn read_refs_fixture_sources(
    repo_root: &Path,
    fixture_path: &Path,
    max_count: usize,
) -> Result<Vec<ImportSource>> {
    let content = fs::read_to_string(fixture_path).map_err(|e| {
        Error::storage(format!("read refs fixture {}: {e}", fixture_path.display()))
    })?;
    let items = parse_refs_fixture_items(&content)?;
    let mut sources = Vec::new();

    for item in items.into_iter().take(max_count) {
        let identity = fixture_item_identity(&item)?;
        let kind = normalize_refs_kind(&item.kind)?;
        let source_ref_root = format!(
            "refs:{}:{}:{}",
            sanitize_source_ref_component(item.repo.trim()),
            sanitize_source_ref_component(&kind),
            sanitize_source_ref_component(&identity),
        );
        let source_metadata = json!({
            "origin": "git-import-refs-fixture",
            "fixture_path": fixture_path.display().to_string(),
            "repo_root": repo_root.display().to_string(),
            "repo": item.repo.trim(),
            "kind": kind,
            "identity": identity,
            "author_name": item.author.clone(),
            "url": item.url.clone(),
            "authored_at": item.authored_at.clone(),
        });
        let body = item.body.clone().unwrap_or_default();
        sources.push(ImportSource {
            source_kind: "git_refs_fixture",
            source_ref_root,
            commit: None,
            authored_at: item.authored_at,
            body,
            source_metadata,
        });
    }

    Ok(sources)
}

fn parse_refs_fixture_items(content: &str) -> Result<Vec<RefsFixtureItem>> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        serde_json::from_str(trimmed)
            .map_err(|e| Error::invalid_request(format!("invalid refs fixture JSON array: {e}")))
    } else if content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
        > 1
    {
        let mut items = Vec::new();
        for (line_no, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let item: RefsFixtureItem = serde_json::from_str(line).map_err(|e| {
                Error::invalid_request(format!(
                    "invalid refs fixture JSONL at line {}: {e}",
                    line_no + 1
                ))
            })?;
            items.push(item);
        }
        Ok(items)
    } else if trimmed.starts_with('{') {
        let item: RefsFixtureItem = serde_json::from_str(trimmed).map_err(|e| {
            Error::invalid_request(format!("invalid refs fixture JSON object: {e}"))
        })?;
        Ok(vec![item])
    } else {
        Err(Error::invalid_request(
            "refs fixture must be JSON array, JSON object, or JSONL",
        ))
    }
}

fn fixture_item_identity(item: &RefsFixtureItem) -> Result<String> {
    if let Some(value) = &item.number {
        return Ok(normalize_json_value(value));
    }
    if let Some(value) = &item.id {
        return Ok(normalize_json_value(value));
    }
    if let Some(url) = &item.url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    Err(Error::invalid_request(
        "refs fixture item must include number, id, or url",
    ))
}

fn normalize_json_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_string(),
        Value::Number(value) => value.to_string(),
        _ => value.to_string(),
    }
}

fn sanitize_source_ref_component(value: &str) -> String {
    value.trim().replace('#', "%23")
}

fn normalize_refs_kind(kind: &str) -> Result<String> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "commit" => Ok("commit".to_string()),
        "issue" => Ok("issue".to_string()),
        "pr" | "pull_request" | "pull-request" => Ok("pr".to_string()),
        "review_comment" | "review-comment" => Ok("review_comment".to_string()),
        _ => Err(Error::invalid_request(
            "unsupported refs fixture kind; use commit, pr, issue, or review_comment",
        )),
    }
}

fn source_ref_for_trailer(source: &ImportSource, trailer: &str, index: usize) -> String {
    if source.source_kind == "git_commit_trailer" {
        return format!(
            "git:{}:{}:{}",
            source.source_ref_root,
            trailer.to_ascii_lowercase(),
            index
        );
    }
    format!("{}#{}", source.source_ref_root, index)
}

fn source_metadata_with_trailer(source_metadata: &Value, trailer: &str) -> Value {
    let mut metadata = source_metadata.clone();
    if let Some(object) = metadata.as_object_mut() {
        object.insert("trailer".to_string(), Value::String(trailer.to_string()));
    }
    metadata
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
