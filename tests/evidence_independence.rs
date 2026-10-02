//! Conclusions retain useful memory without proving independent user adoption.

use codex_memoryd::config::Config;
use codex_memoryd::portable_bundle::{self, BundleExportOptions, BundleImportOptions};
use codex_memoryd::protocol::*;
use codex_memoryd::service::Service;
use codex_memoryd::store::Store;
use serde_json::json;

fn service() -> Service {
    Service::new(Store::open(":memory:").unwrap(), Config::default())
}

fn mirror(service: &Service, metadata: Option<serde_json::Value>, content: &str) -> String {
    let result = service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![content.into()]),
            metadata,
            record_type: Some("decision".into()),
        })
        .unwrap();
    result.record_ids[0].clone()
}

fn preview(service: &Service) -> DreamResponse {
    service
        .dream(DreamRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            mode: Some("preview".into()),
            now: Some("2026-10-02T00:00:00Z".into()),
            since: None,
        })
        .unwrap()
}

fn recall(service: &Service) -> RecallResponse {
    service
        .recall(RecallRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            session: None,
            query: Some("cargo test validation".into()),
            files: vec![],
            max_tokens: Some(2000),
            pack_mode: None,
            include_types: vec![],
            exclude_types: vec![],
            recency_days: None,
            as_of: None,
            include_history: false,
            metadata: None,
        })
        .unwrap()
}

#[test]
fn builtin_memory_mirror_is_not_independent_user_evidence() {
    let service = service();
    let id = mirror(
        &service,
        Some(json!({
            "actor": "agent:friday",
            "source_kind": "hermes_native_memory",
            "session_id": "mirror-session",
        })),
        "Decision: cargo test is the supported validation command.",
    );
    let original = service.store.get_record(&id).unwrap().unwrap();
    let report = preview(&service);
    assert_eq!(report.evidence_window.conclusions.count, 1);
    assert_eq!(
        report.evidence_window.conclusions.sources[0]
            .actor
            .as_deref(),
        Some("agent:friday")
    );
    assert!(
        !report.candidates.is_empty(),
        "weak evidence remains inspectable"
    );
    for candidate in &report.candidates {
        assert!(
            !candidate.apply_eligible,
            "a mirror must not promote itself"
        );
        assert_eq!(candidate.user_evidence_count, 0);
        assert!(!candidate
            .evidence_classes
            .iter()
            .any(|class| class == "explicit_conclusion"));
        assert_eq!(
            candidate.evidence_refs[0].actor.as_deref(),
            Some("agent:friday")
        );
    }
    assert_eq!(service.store.get_record(&id).unwrap().unwrap(), original);
}

#[test]
fn unknown_legacy_conclusion_keeps_recall_without_inventing_adoption() {
    let service = service();
    let id = mirror(
        &service,
        None,
        "Decision: cargo test is the supported validation command.",
    );
    assert_eq!(recall(&service).facts.len(), 1);
    let original = service.store.get_record(&id).unwrap().unwrap();
    let report = preview(&service);
    assert_eq!(report.evidence_window.conclusions.count, 1);
    assert!(report.evidence_window.conclusions.sources[0]
        .actor
        .is_none());
    assert!(!report.candidates.is_empty());
    for candidate in &report.candidates {
        assert!(!candidate.apply_eligible);
        assert_eq!(candidate.user_evidence_count, 0);
        assert_eq!(candidate.evidence_weight, 0.0);
        assert!(
            candidate.evidence_refs[0].actor.is_none(),
            "target is not speaker attribution"
        );
    }
    assert_eq!(service.store.get_record(&id).unwrap().unwrap(), original);
    assert_eq!(recall(&service).facts.len(), 1);
}

#[test]
fn caller_user_labels_do_not_prove_primary_adoption() {
    let service = service();
    mirror(
        &service,
        Some(
            json!({"actor": "user", "source_kind": "explicit_user_confirmation", "write_origin": "user"}),
        ),
        "Decision: cargo test is the supported validation command.",
    );
    let report = preview(&service);
    assert!(!report.candidates.is_empty());
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
}

#[test]
fn recalled_memory_round_trip_has_zero_promotion_weight() {
    let service = service();
    mirror(
        &service,
        None,
        "Decision: cargo test is the supported validation command.",
    );
    let recalled = recall(&service);
    assert_eq!(recalled.facts.len(), 1);
    let id = mirror(
        &service,
        Some(
            json!({"actor": "agent:other-host", "source_kind": "memoryd_derived", "write_origin": "memoryd_recall", "session_id": "another-session"}),
        ),
        "Decision: the supported validation command remains cargo test.",
    );
    let original = service.store.get_record(&id).unwrap().unwrap();
    let report = preview(&service);
    assert!(!report.candidates.is_empty());
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible
            && candidate.evidence_weight == 0.0
            && candidate.user_evidence_count == 0));
    assert_eq!(service.store.get_record(&id).unwrap().unwrap(), original);
}

#[test]
fn bundle_round_trip_preserves_the_mirror_evidence_ceiling() {
    let dir = tempfile::TempDir::new().unwrap();
    let source = service();
    mirror(
        &source,
        Some(json!({"actor": "agent:friday", "source_kind": "hermes_builtin_memory_import"})),
        "Decision: cargo test is the supported validation command.",
    );
    let bundle = dir.path().join("mirrored.cmembundle");
    portable_bundle::export_write(
        &source.store,
        &BundleExportOptions {
            profile: "personal".into(),
            workspace: "independence".into(),
            repo_id: None,
            record_ids: vec![],
            include_archived: false,
            target_profile: "personal".into(),
            target_workspace: Some("independence".into()),
            target_repo_id: None,
            created_at: None,
        },
        &bundle,
    )
    .unwrap();
    let destination = service();
    let options = BundleImportOptions {
        profile: "personal".into(),
        workspace: "independence".into(),
        repo_id: None,
    };
    let plan = portable_bundle::import_preview(&destination.store, &bundle, &options).unwrap();
    portable_bundle::import_apply(
        &destination.store,
        &bundle,
        &options,
        plan.plan_id.as_deref().unwrap(),
    )
    .unwrap();
    let facts = recall(&destination).facts;
    assert_eq!(facts.len(), 1);
    assert_eq!(
        facts[0].policy.provenance.actor.as_deref(),
        Some("agent:friday")
    );
    let report = preview(&destination);
    assert!(!report.candidates.is_empty());
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
    let reexport = dir.path().join("reexport.cmembundle");
    portable_bundle::export_write(
        &destination.store,
        &BundleExportOptions {
            profile: "personal".into(),
            workspace: "independence".into(),
            repo_id: None,
            record_ids: vec![],
            include_archived: false,
            target_profile: "personal".into(),
            target_workspace: Some("independence".into()),
            target_repo_id: None,
            created_at: None,
        },
        &reexport,
    )
    .unwrap();
    let next = service();
    let plan = portable_bundle::import_preview(&next.store, &reexport, &options).unwrap();
    portable_bundle::import_apply(
        &next.store,
        &reexport,
        &options,
        plan.plan_id.as_deref().unwrap(),
    )
    .unwrap();
    assert!(preview(&next)
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
}

#[test]
fn separately_captured_user_evidence_remains_primary() {
    let service = service();
    mirror(
        &service,
        Some(json!({"actor": "agent:friday", "source_kind": "hermes_native_memory"})),
        "Decision: cargo test is the supported validation command.",
    );
    service
        .turns(TurnsRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            session: Some(TurnSession {
                id: Some("user-capture".into()),
                thread_id: None,
                source: Some("test-visible-capture".into()),
                metadata: None,
            }),
            messages: Some(vec![
                TurnMessage {
                    actor: "user".into(),
                    content: "Use cargo test as the supported validation command.".into(),
                    created_at: Some("2026-10-01T10:00:00Z".into()),
                    metadata: None,
                },
                TurnMessage {
                    actor: "user".into(),
                    content: "Run cargo test as the supported validation command.".into(),
                    created_at: Some("2026-10-01T10:01:00Z".into()),
                    metadata: None,
                },
            ]),
            write_policy: None,
        })
        .unwrap();
    let report = preview(&service);
    assert!(report
        .candidates
        .iter()
        .any(|candidate| candidate.apply_eligible
            && candidate.user_evidence_count == 2
            && candidate
                .evidence_classes
                .iter()
                .any(|class| class == "user_visible_turn")));
}

#[test]
fn legacy_top_level_and_nested_attribution_cannot_upgrade_conclusions() {
    // Declared compatibility variants of retained pre-change metadata. Source
    // bytes and timestamps remain unchanged; no historical event is invented.
    for metadata in [
        json!({"origin": "conclusion", "target": "user", "actor": "assistant", "source_kind": "host_native_recall"}),
        json!({"origin": "conclusion", "target": "user", "provenance": {"actor": "agent:legacy", "source_kind": "memoryd_derived"}}),
        json!({"origin": "conclusion", "target": "user", "derived_from_memory_ids": ["unknown-retained-reference"]}),
    ] {
        let service = service();
        let id = mirror(
            &service,
            None,
            "Decision: cargo test is the supported validation command.",
        );
        service
            .store
            .transaction_immediate(|tx| {
                tx.execute(
                    "UPDATE memory_records SET metadata = ?1 WHERE id = ?2",
                    rusqlite::params![metadata.to_string(), id],
                )?;
                Ok(())
            })
            .unwrap();
        let original = service.store.get_record(&id).unwrap().unwrap();
        let report = preview(&service);
        assert!(!report.candidates.is_empty());
        assert!(report
            .candidates
            .iter()
            .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
        assert_eq!(service.store.get_record(&id).unwrap().unwrap(), original);
    }
}

#[test]
fn mixed_non_primary_sources_on_different_days_cannot_promote() {
    let service = service();
    // Manually constructed retained shapes with declared synthetic historical
    // creation times. These are not new observations or verification dates.
    for (index, metadata, content) in [
        (
            0,
            json!({"origin": "conclusion", "provenance": {"actor": "agent:first-host", "source_kind": "hermes_native_memory"}}),
            "Decision: cargo test is the supported validation command.",
        ),
        (
            1,
            json!({"origin": "codex-local-memory", "artifact_kind": "memory_summary"}),
            "Decision: use cargo test as the supported validation command.",
        ),
        (
            2,
            json!({"origin": "conclusion"}),
            "Decision: the supported validation command remains cargo test.",
        ),
    ] {
        let id = mirror(&service, None, content);
        let date = format!("2026-09-0{}T10:00:00Z", index + 1);
        service.store.transaction_immediate(|tx| {
            tx.execute("UPDATE memory_records SET metadata = ?1, created_at = ?2, updated_at = ?2 WHERE id = ?3", rusqlite::params![metadata.to_string(), date, id])?;
            Ok(())
        }).unwrap();
    }
    let report = preview(&service);
    assert!(!report.candidates.is_empty());
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
}

#[test]
fn assistant_and_imported_evidence_cannot_use_dates_as_user_steering() {
    let service = service();
    // Declared pre-change record shapes isolate the former distinct-days
    // branch, without any conclusion being mistaken for primary evidence.
    for (date, metadata, content) in [
        (
            "2026-09-01T10:00:00Z",
            json!({"origin": "visible_turn", "actor": "assistant"}),
            "Decision: cargo test is the supported validation command.",
        ),
        (
            "2026-09-02T10:00:00Z",
            json!({"origin": "codex-local-memory", "artifact_kind": "memory_summary"}),
            "Decision: use cargo test as the supported validation command.",
        ),
    ] {
        let id = mirror(&service, None, content);
        service.store.transaction_immediate(|tx| {
            tx.execute("UPDATE memory_records SET metadata = ?1, created_at = ?2, updated_at = ?2 WHERE id = ?3", rusqlite::params![metadata.to_string(), date, id])?;
            Ok(())
        }).unwrap();
    }
    let report = preview(&service);
    let mixed = report
        .candidates
        .iter()
        .find(|candidate| {
            candidate
                .evidence_classes
                .contains(&"assistant_visible_turn".into())
                && candidate
                    .evidence_classes
                    .contains(&"imported_memory".into())
        })
        .expect("both weak source classes must reach the same scoring group");
    assert_eq!(mixed.user_evidence_count, 0);
    assert!(!mixed.apply_eligible);
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible));
}

#[test]
fn copied_nested_user_attribution_does_not_invent_visible_capture() {
    let service = service();
    // Bundle-compatible attribution is a retained claim about its source,
    // not proof that this writer captured a new visible user event.
    for content in [
        "Decision: cargo test is the supported validation command.",
        "Decision: use cargo test as the supported validation command.",
    ] {
        let id = mirror(&service, None, content);
        service
            .store
            .transaction_immediate(|tx| {
                tx.execute(
                    "UPDATE memory_records SET metadata = ?1 WHERE id = ?2",
                    rusqlite::params![
                        json!({"origin": "visible_turn", "provenance": {"actor": "user"}})
                            .to_string(),
                        id
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }
    assert_eq!(recall(&service).facts.len(), 2);
    let report = preview(&service);
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
}

#[test]
fn imported_nested_user_labels_do_not_gain_primary_weight() {
    let dir = tempfile::TempDir::new().unwrap();
    let source = service();
    let mut originals = Vec::new();
    for content in [
        "Decision: cargo test is the supported validation command.",
        "Decision: use cargo test as the supported validation command.",
    ] {
        let id = mirror(&source, None, content);
        source
            .store
            .transaction_immediate(|tx| {
                tx.execute(
                    "UPDATE memory_records SET metadata = ?1 WHERE id = ?2",
                    rusqlite::params![
                        json!({"origin": "visible_turn", "provenance": {"actor": "user"}})
                            .to_string(),
                        id
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        originals.push(source.store.get_record(&id).unwrap().unwrap());
    }
    let bundle = dir.path().join("unverified-attribution.cmembundle");
    portable_bundle::export_write(
        &source.store,
        &BundleExportOptions {
            profile: "personal".into(),
            workspace: "independence".into(),
            repo_id: None,
            record_ids: vec![],
            include_archived: false,
            target_profile: "personal".into(),
            target_workspace: Some("independence".into()),
            target_repo_id: None,
            created_at: None,
        },
        &bundle,
    )
    .unwrap();
    let destination = service();
    let options = BundleImportOptions {
        profile: "personal".into(),
        workspace: "independence".into(),
        repo_id: None,
    };
    let plan = portable_bundle::import_preview(&destination.store, &bundle, &options).unwrap();
    portable_bundle::import_apply(
        &destination.store,
        &bundle,
        &options,
        plan.plan_id.as_deref().unwrap(),
    )
    .unwrap();
    assert_eq!(recall(&destination).facts.len(), 2);
    let imported = destination
        .store
        .query_records(&codex_memoryd::store::RecordQuery {
            profile_id: Some("personal".into()),
            workspace_id: Some("independence".into()),
            ..Default::default()
        })
        .unwrap();
    for record in &imported {
        let original = originals
            .iter()
            .find(|source| source.content == record.content)
            .unwrap();
        assert_eq!(record.metadata["provenance"]["actor"], "user");
        assert_eq!(record.created_at, original.created_at);
        assert_eq!(record.updated_at, original.updated_at);
        assert_eq!(record.observed_at, original.observed_at);
    }
    let report = preview(&destination);
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.user_evidence_count == 0));
}

#[test]
fn conflicting_labels_cannot_hide_known_derivation() {
    for labels in [
        json!({"source_kind": "memoryd_derived", "provenance": {"source_kind": "unknown"}}),
        json!({"source_kind": "unknown", "provenance": {"source_kind": "memoryd_derived"}}),
        json!({"write_origin": "memoryd_recall", "provenance": {"write_origin": "unknown"}}),
        json!({"write_origin": "unknown", "provenance": {"write_origin": "memoryd_recall"}}),
    ] {
        let service = service();
        for content in [
            "Decision: cargo test is the supported validation command.",
            "Decision: use cargo test as the supported validation command.",
        ] {
            let id = mirror(&service, None, content);
            let mut metadata = labels.clone();
            metadata["origin"] = json!("visible_turn");
            metadata["actor"] = json!("user");
            service
                .store
                .transaction_immediate(|tx| {
                    tx.execute(
                        "UPDATE memory_records SET metadata = ?1 WHERE id = ?2",
                        rusqlite::params![metadata.to_string(), id],
                    )?;
                    Ok(())
                })
                .unwrap();
        }
        let report = preview(&service);
        assert!(!report.candidates.is_empty());
        assert!(report
            .candidates
            .iter()
            .all(|candidate| !candidate.apply_eligible
                && candidate.user_evidence_count == 0
                && candidate.evidence_weight == 0.0));
    }
}

#[test]
fn actual_provenance_backfill_cannot_raise_legacy_derivative_weight() {
    let service = service();
    let content = "Decision: cargo test is the supported validation command.";
    let id = mirror(
        &service,
        Some(json!({"actor": "assistant", "source_kind": "hermes_native_memory"})),
        content,
    );
    let mut original = service.store.get_record(&id).unwrap().unwrap();
    original
        .metadata
        .as_object_mut()
        .unwrap()
        .remove("provenance");
    original.metadata["actor"] = json!("assistant");
    original.metadata["source_kind"] = json!("memoryd_derived");
    service
        .store
        .transaction_immediate(|tx| {
            tx.execute(
                "UPDATE memory_records SET metadata = ?1 WHERE id = ?2",
                rusqlite::params![original.metadata.to_string(), id],
            )?;
            Ok(())
        })
        .unwrap();
    let repeated = service
        .conclusions(ConclusionsRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            target: Some("user".into()),
            conclusions: Some(vec![content.into()]),
            metadata: Some(json!({"actor": "assistant", "source_kind": "hermes_native_memory"})),
            record_type: Some("decision".into()),
        })
        .unwrap();
    assert!(repeated.record_ids.is_empty());
    let retained = service.store.get_record(&id).unwrap().unwrap();
    assert_eq!(retained.metadata["source_kind"], "memoryd_derived");
    assert_eq!(
        retained.metadata["provenance"]["source_kind"],
        "hermes_native_memory"
    );
    assert_eq!(retained.created_at, original.created_at);
    assert_eq!(retained.updated_at, original.updated_at);
    assert_eq!(retained.observed_at, original.observed_at);
    let report = preview(&service);
    assert!(!report.candidates.is_empty());
    assert!(report
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible && candidate.evidence_weight == 0.0));
}

#[test]
fn later_weak_contradiction_cannot_supply_a_primary_backed_statement() {
    let service = service();
    service
        .turns(TurnsRequest {
            profile: Some("personal".into()),
            workspace: Some("independence".into()),
            repo: None,
            session: Some(TurnSession {
                id: Some("primary-capture".into()),
                thread_id: None,
                source: Some("test-visible-capture".into()),
                metadata: None,
            }),
            messages: Some(vec![
                TurnMessage {
                    actor: "user".into(),
                    content: "Use cargo test as the supported validation command.".into(),
                    created_at: None,
                    metadata: None,
                },
                TurnMessage {
                    actor: "user".into(),
                    content: "Run cargo test as the supported validation command.".into(),
                    created_at: None,
                    metadata: None,
                },
            ]),
            write_policy: None,
        })
        .unwrap();
    let before = preview(&service);
    let primary = before
        .candidates
        .iter()
        .find(|candidate| candidate.apply_eligible && candidate.user_evidence_count == 2)
        .unwrap();
    let id = mirror(
        &service,
        Some(json!({"actor": "agent:other-host", "source_kind": "memoryd_derived"})),
        "Decision: never use cargo test as the supported validation command.",
    );
    let mut weak = service.store.get_record(&id).unwrap().unwrap();
    weak.metadata["state"] = json!("completed");
    service
        .store
        .transaction_immediate(|tx| {
            tx.execute(
                "UPDATE memory_records SET metadata = ?1, confidence = 0.99 WHERE id = ?2",
                rusqlite::params![weak.metadata.to_string(), id],
            )?;
            Ok(())
        })
        .unwrap();
    let after = preview(&service);
    assert!(after
        .evidence_window
        .conclusions
        .sources
        .iter()
        .any(|source| source
            .content
            .as_deref()
            .is_some_and(|content| content.contains("never use cargo test"))));
    let accepted = after
        .candidates
        .iter()
        .find(|candidate| candidate.apply_eligible && candidate.user_evidence_count == 2)
        .unwrap();
    assert_eq!(accepted.content, primary.content);
    assert_eq!(accepted.proposed_type, primary.proposed_type);
    assert_eq!(accepted.state, primary.state);
    assert_eq!(accepted.confidence, primary.confidence);
    assert_eq!(accepted.first_seen_at, primary.first_seen_at);
    assert_eq!(accepted.last_seen_at, primary.last_seen_at);
    assert!(after
        .candidates
        .iter()
        .all(|candidate| !candidate.apply_eligible
            || !candidate.content.contains("never use cargo test")));
}
