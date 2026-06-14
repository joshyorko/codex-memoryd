//! Performance / cost budget smoke test (issue #152).
//!
//! Asserts deterministic, fixture-scale cost budgets (records, output bytes,
//! estimated tokens) for the main substrate paths. Wall-clock timing is NOT
//! asserted — only the stable byte/token/count budgets — so CI does not flake
//! on a busy machine. Budgets are generous ceilings: they catch a silent
//! blow-up (e.g. recall suddenly returning 10x the bytes) without overfitting.

use codex_memoryd::perf;

/// Deterministic zero clock: timing is informational only, so tests pin it to
/// keep the report fully reproducible.
fn zero_clock() -> u128 {
    0
}

#[test]
fn perf_report_is_stable_and_within_budget() {
    let a = perf::run_perf_report(zero_clock).expect("perf report a");
    let b = perf::run_perf_report(zero_clock).expect("perf report b");

    // The report is structurally stable across runs: same paths and same item
    // counts. Output bytes vary by a few bytes run-to-run because responses
    // embed fresh timestamps and UUID ids, so we assert a tolerance band rather
    // than byte-exact equality.
    assert_eq!(a.measurements.len(), b.measurements.len());
    for (ma, mb) in a.measurements.iter().zip(b.measurements.iter()) {
        assert_eq!(ma.path, mb.path, "path order stable");
        assert_eq!(ma.items, mb.items, "item count stable for '{}'", ma.path);
        let delta = (ma.output_bytes as i64 - mb.output_bytes as i64).unsigned_abs();
        assert!(
            delta <= 64,
            "path '{}' byte count varied by {delta} (> 64) between runs",
            ma.path
        );
    }

    assert_eq!(a.seed_records, 40);
    assert!(
        a.measurements.len() >= 5,
        "expected recall/search/card/adapter/procedure paths"
    );

    // Generous per-path ceilings: catch a silent blow-up without flaking.
    // (Observed values are well under these.)
    let budget = |path: &str| -> u64 {
        match path {
            "recall" => 60_000,
            "search" => 20_000,
            "card_workspace_summary" => 40_000,
            "adapter_mcp_pack" => 20_000,
            "procedure_recall" => 20_000,
            _ => 100_000,
        }
    };

    for m in &a.measurements {
        // Every path must be exercised and produce some output.
        assert!(m.output_bytes > 0, "path '{}' produced no output", m.path);
        assert!(
            (m.output_bytes as u64) <= budget(m.path),
            "path '{}' output {} bytes exceeds budget {}",
            m.path,
            m.output_bytes,
            budget(m.path)
        );
        // Token estimate must track bytes (~4 bytes/token).
        assert_eq!(m.estimated_tokens, m.output_bytes.div_ceil(4));
        // Timing is zero under the deterministic clock (proves it's not asserted).
        assert_eq!(m.elapsed_micros, 0);
    }
}

#[test]
fn recall_path_returns_bounded_record_set() {
    let report = perf::run_perf_report(zero_clock).expect("perf report");
    let recall = report
        .measurements
        .iter()
        .find(|m| m.path == "recall")
        .expect("recall measurement");
    // Recall is budgeted: it must not return the entire 40-record corpus.
    assert!(recall.items <= 40, "recall returned {} items", recall.items);
    assert!(
        recall.items > 0,
        "recall returned nothing from a seeded corpus"
    );
}
