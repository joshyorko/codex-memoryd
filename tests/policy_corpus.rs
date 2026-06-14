//! Policy regression corpus runner (issue #142).
//!
//! Loads the allow/deny/redaction fixtures under `tests/fixtures/policy/` and
//! runs each case through the real `codex_memoryd::policy` functions — the same
//! gate the daemon, CLI, and ingest paths use. The goal is durable regression
//! coverage: every dogfood false positive (safe content wrongly blocked) and
//! false negative (unsafe content wrongly admitted) becomes a permanent case
//! here instead of a one-off fix.
//!
//! The test prints per-category counts so the corpus size is visible in CI.

use std::path::PathBuf;

use codex_memoryd::policy;
use codex_memoryd::policy::PolicyDecision;
use serde::Deserialize;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("policy")
}

fn load<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = fixtures_dir().join(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

#[derive(Debug, Deserialize)]
struct AllowCorpus {
    cases: Vec<AllowCase>,
}

#[derive(Debug, Deserialize)]
struct AllowCase {
    name: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct DenyCorpus {
    cases: Vec<DenyCase>,
}

#[derive(Debug, Deserialize)]
struct DenyCase {
    name: String,
    /// Literal content, OR `content_parts` for secret-shaped cases.
    #[serde(default)]
    content: Option<String>,
    /// Fragments joined (no separator) to form the content. Used so the
    /// committed fixture never holds a contiguous secret-shaped literal that
    /// would trip push protection — the policy gate still sees the full string.
    #[serde(default)]
    content_parts: Option<Vec<String>>,
    #[serde(default)]
    expect_code: Option<String>,
}

impl DenyCase {
    fn content(&self) -> String {
        join_content(&self.name, &self.content, &self.content_parts)
    }
}

#[derive(Debug, Deserialize)]
struct RedactCorpus {
    cases: Vec<RedactCase>,
}

#[derive(Debug, Deserialize)]
struct RedactCase {
    name: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    content_parts: Option<Vec<String>>,
    /// Raw values that must NOT survive redaction, each as joined fragments.
    must_not_contain_parts: Vec<Vec<String>>,
}

impl RedactCase {
    fn content(&self) -> String {
        join_content(&self.name, &self.content, &self.content_parts)
    }
    fn must_not_contain(&self) -> Vec<String> {
        self.must_not_contain_parts
            .iter()
            .map(|parts| parts.concat())
            .collect()
    }
}

/// Resolve a case's effective content from either a literal `content` or
/// fragment `content_parts`.
fn join_content(name: &str, content: &Option<String>, parts: &Option<Vec<String>>) -> String {
    match (content, parts) {
        (Some(s), _) => s.clone(),
        (None, Some(p)) => p.concat(),
        (None, None) => panic!("case '{name}' has neither content nor content_parts"),
    }
}

#[test]
fn allow_cases_pass_the_gate() {
    let corpus: AllowCorpus = load("allow.json");
    assert!(!corpus.cases.is_empty(), "allow corpus must not be empty");
    for case in &corpus.cases {
        // Safe content must not be detected as a secret...
        assert!(
            policy::detect_secret(&case.content).is_none(),
            "allow case '{}' was flagged as a secret: {:?}",
            case.name,
            policy::detect_secret(&case.content)
        );
        // ...and must be accepted by the full string gate.
        match policy::screen_string_value(&case.content) {
            PolicyDecision::Accept(_) => {}
            PolicyDecision::Reject { code, reason } => {
                panic!("allow case '{}' was rejected ({code}): {reason}", case.name)
            }
        }
    }
    eprintln!("policy corpus: allow cases passed = {}", corpus.cases.len());
}

#[test]
fn deny_cases_are_rejected() {
    let corpus: DenyCorpus = load("deny.json");
    assert!(!corpus.cases.is_empty(), "deny corpus must not be empty");
    for case in &corpus.cases {
        match policy::screen_string_value(&case.content()) {
            PolicyDecision::Accept(_) => {
                panic!(
                    "deny case '{}' was accepted but must be rejected",
                    case.name
                )
            }
            PolicyDecision::Reject { code, reason } => {
                if let Some(expected) = &case.expect_code {
                    assert_eq!(
                        &code, expected,
                        "deny case '{}' rejected with code '{code}' ({reason}), expected '{expected}'",
                        case.name
                    );
                }
                // The rejection reason carries a label, never the raw value: the
                // case content is synthetic, so we only assert a code is present.
                assert!(!code.is_empty(), "deny case '{}' missing code", case.name);
            }
        }
    }
    eprintln!(
        "policy corpus: deny cases rejected = {}",
        corpus.cases.len()
    );
}

#[test]
fn redact_cases_strip_raw_values() {
    let corpus: RedactCorpus = load("redact.json");
    assert!(!corpus.cases.is_empty(), "redact corpus must not be empty");
    for case in &corpus.cases {
        let (redacted, changed) = policy::redact_secret_like(&case.content());
        assert!(
            changed,
            "redact case '{}' reported no redaction; expected at least one",
            case.name
        );
        for needle in &case.must_not_contain() {
            assert!(
                !redacted.contains(needle.as_str()),
                "redact case '{}' leaked raw value in output",
                case.name
            );
        }
        // The redaction marker must be present so the summary is auditable.
        assert!(
            redacted.contains("[redacted:"),
            "redact case '{}' produced no redaction marker",
            case.name
        );
    }
    eprintln!(
        "policy corpus: redact cases stripped = {}",
        corpus.cases.len()
    );
}
