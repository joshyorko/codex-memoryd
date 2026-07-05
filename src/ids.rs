//! Stable identifiers, content/source hashing, and timestamp helpers
//! (SPEC §4.2).

use sha2::Digest;
use sha2::Sha256;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const PUBLIC_HANDLE_SUFFIX_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicHandleKind {
    MemoryRef,
    SourceRef,
    SubjectRef,
    EpisodeRef,
    CheckpointRef,
}

impl PublicHandleKind {
    fn prefix(self) -> &'static str {
        match self {
            PublicHandleKind::MemoryRef => "mr_",
            PublicHandleKind::SourceRef => "msrc_",
            PublicHandleKind::SubjectRef => "msub_",
            PublicHandleKind::EpisodeRef => "mep_",
            PublicHandleKind::CheckpointRef => "mcp_",
        }
    }
}

/// RFC3339 timestamp for "now" in UTC. All stored timestamps are RFC3339 UTC
/// strings so they sort lexicographically and round-trip cleanly through JSON.
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

/// Generate a fresh prefixed unique id, e.g. `mem_<uuid>`.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

/// Lowercase hex sha256 of the given bytes, prefixed `sha256:`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(7 + digest.len() * 2);
    out.push_str("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Produce a one-way public handle for values that may cross the API boundary.
/// These handles are intentionally opaque and carry no authority.
pub fn public_handle(kind: PublicHandleKind, raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.prefix().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(raw.trim().as_bytes());
    let digest = hasher.finalize();
    let mut suffix = String::with_capacity(PUBLIC_HANDLE_SUFFIX_LEN);
    for byte in digest {
        suffix.push_str(&format!("{byte:02x}"));
        if suffix.len() >= PUBLIC_HANDLE_SUFFIX_LEN {
            suffix.truncate(PUBLIC_HANDLE_SUFFIX_LEN);
            break;
        }
    }
    format!("{}{}", kind.prefix(), suffix)
}

pub fn is_valid_public_handle(value: &str) -> bool {
    parse_public_handle(value).is_some()
}

pub fn parse_public_handle(value: &str) -> Option<PublicHandleKind> {
    for kind in [
        PublicHandleKind::MemoryRef,
        PublicHandleKind::SourceRef,
        PublicHandleKind::SubjectRef,
        PublicHandleKind::EpisodeRef,
        PublicHandleKind::CheckpointRef,
    ] {
        if let Some(suffix) = value.strip_prefix(kind.prefix()) {
            return (suffix.len() == PUBLIC_HANDLE_SUFFIX_LEN
                && suffix.chars().all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()))
            .then_some(kind);
        }
    }
    None
}

/// Normalize content for hashing: trim, collapse internal whitespace runs, and
/// lowercase. This makes near-identical chunks dedupe to the same content hash.
fn normalize_for_hash(content: &str) -> String {
    let mut normalized = String::with_capacity(content.len());
    let mut last_was_space = false;
    for ch in content.trim().chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        } else {
            for lower in ch.to_lowercase() {
                normalized.push(lower);
            }
            last_was_space = false;
        }
    }
    normalized
}

/// Content hash binds normalized content to its identity context
/// (profile/workspace/repo/type/scope), per SPEC §4.2. Two records with the
/// same content but different scope are NOT duplicates.
pub fn content_hash(
    profile: &str,
    workspace: &str,
    repo_id: Option<&str>,
    record_type: &str,
    scope: &str,
    content: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(profile.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(workspace.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(repo_id.unwrap_or("").as_bytes());
    hasher.update(b"\x1f");
    hasher.update(record_type.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(scope.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(normalize_for_hash(content).as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Source hash binds raw imported content to its source path and
/// profile/workspace, per SPEC §4.2. Used to short-circuit unchanged re-imports.
pub fn source_hash(profile: &str, workspace: &str, source_path: &str, raw_content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(profile.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(workspace.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(source_path.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(raw_content.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::from("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_hash_is_scope_sensitive() {
        let a = content_hash("personal", "ws", None, "decision", "repo", "Use axum");
        let b = content_hash("personal", "ws", None, "decision", "file", "Use axum");
        assert_ne!(a, b, "different scope must yield different content hash");
    }

    #[test]
    fn content_hash_normalizes_whitespace_and_case() {
        let a = content_hash("p", "w", None, "t", "s", "Use   Axum\n");
        let b = content_hash("p", "w", None, "t", "s", "use axum");
        assert_eq!(a, b, "whitespace/case normalization must collapse");
    }

    #[test]
    fn ids_are_prefixed_and_unique() {
        let a = new_id("mem");
        let b = new_id("mem");
        assert!(a.starts_with("mem_"));
        assert_ne!(a, b);
    }

    #[test]
    fn public_handles_are_stable_and_parseable() {
        let handle = public_handle(PublicHandleKind::MemoryRef, "mem_example");
        assert!(handle.starts_with("mr_"));
        assert_eq!(parse_public_handle(&handle), Some(PublicHandleKind::MemoryRef));
        assert!(is_valid_public_handle(&handle));
    }

    #[test]
    fn public_handles_reject_non_conforming_values() {
        assert!(!is_valid_public_handle("mem_example"));
        assert!(!is_valid_public_handle("mr_../../etc/shadow"));
        assert!(!is_valid_public_handle("mr_ABCDEF0123456789abcdef0123456789"));
    }
}
