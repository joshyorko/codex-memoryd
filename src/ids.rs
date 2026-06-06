//! Stable identifiers, content/source hashing, and timestamp helpers
//! (SPEC §4.2).

use sha2::Digest;
use sha2::Sha256;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

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
}
