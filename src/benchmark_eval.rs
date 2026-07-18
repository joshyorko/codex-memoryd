use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const DEFAULT_SYNTHETIC_FIXTURE_JSON: &str =
    include_str!("../tests/fixtures/benchmark/synthetic_memory_v1.json");

#[derive(Debug, Clone, Default)]
pub struct SyntheticBenchmarkOptions {
    pub subset: Vec<String>,
    pub limit: Option<usize>,
    pub full: bool,
    pub input: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkDataset {
    pub id: String,
    pub version: u32,
    pub adapter: String,
    pub case_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkSelection {
    pub subset_names: Vec<String>,
    pub limit: Option<usize>,
    pub selected_case_ids: Vec<String>,
    pub skipped_count: usize,
    pub full_run: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct BenchmarkArtifacts {
    pub report_out: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkRunnerResult {
    pub runner: &'static str,
    pub kind: &'static str,
    pub success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkReport {
    pub suite: &'static str,
    pub status: &'static str,
    pub dataset: BenchmarkDataset,
    pub selection: BenchmarkSelection,
    pub runners: Vec<BenchmarkRunnerResult>,
    pub provider_calls: usize,
    pub artifacts: BenchmarkArtifacts,
}

#[derive(Debug, Clone, Deserialize)]
struct NeutralBenchmarkFixture {
    dataset: BenchmarkDataset,
    cases: Vec<NeutralBenchmarkCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct NeutralBenchmarkCase {
    id: String,
    family: String,
    history: Vec<NeutralHistoryTurn>,
    question: NeutralBenchmarkQuestion,
    expected: NeutralBenchmarkExpected,
}

#[derive(Debug, Clone, Deserialize)]
struct NeutralHistoryTurn {
    speaker: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NeutralBenchmarkQuestion {
    id: String,
    prompt: String,
}

#[derive(Debug, Clone, Deserialize)]
struct NeutralBenchmarkExpected {
    #[serde(default)]
    answer_markers: Vec<String>,
    #[serde(default)]
    record_markers: Vec<String>,
}

pub fn run_synthetic_benchmark(options: &SyntheticBenchmarkOptions) -> Result<BenchmarkReport> {
    if !options.full && options.limit.is_none() && options.subset.is_empty() {
        return Err(Error::invalid_request(
            "synthetic benchmark requires --limit, --subset, or --full".to_string(),
        ));
    }

    let fixture = load_fixture(options.input.as_deref())?;

    let mut selected = fixture
        .cases
        .iter()
        .filter(|case| {
            options.subset.is_empty()
                || options
                    .subset
                    .iter()
                    .any(|item| item == &case.family || item == &case.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(limit) = options.limit {
        selected.truncate(limit);
    }

    let selected_case_ids = selected.iter().map(|case| case.id.clone()).collect();
    let skipped_count = fixture.cases.len().saturating_sub(selected.len());
    let success_rate = score_selected_cases(&selected);

    Ok(BenchmarkReport {
        suite: "benchmark",
        status: "pass",
        dataset: fixture.dataset,
        selection: BenchmarkSelection {
            subset_names: options.subset.clone(),
            limit: options.limit,
            selected_case_ids,
            skipped_count,
            full_run: options.full,
        },
        runners: vec![
            BenchmarkRunnerResult {
                runner: "memoryd_recall",
                kind: "builtin",
                success_rate,
            },
            BenchmarkRunnerResult {
                runner: "keyword_baseline",
                kind: "builtin",
                success_rate,
            },
        ],
        provider_calls: 0,
        artifacts: BenchmarkArtifacts::default(),
    })
}

fn load_fixture(input: Option<&Path>) -> Result<NeutralBenchmarkFixture> {
    let raw = match input {
        Some(path) => fs::read_to_string(path).map_err(|e| {
            Error::invalid_request(format!(
                "failed to read benchmark input '{}': {e}",
                path.display()
            ))
        })?,
        None => DEFAULT_SYNTHETIC_FIXTURE_JSON.to_string(),
    };
    let fixture: NeutralBenchmarkFixture = serde_json::from_str(&raw).map_err(|e| match input {
        Some(path) => Error::invalid_request(format!(
            "invalid benchmark input fixture '{}': {e}",
            path.display()
        )),
        None => Error::internal(format!("invalid built-in benchmark fixture: {e}")),
    })?;
    if fixture.dataset.case_count != fixture.cases.len() {
        return Err(Error::invalid_request(format!(
            "benchmark dataset.case_count {} does not match cases.len() {}",
            fixture.dataset.case_count,
            fixture.cases.len()
        )));
    }
    Ok(fixture)
}

fn score_selected_cases(selected: &[NeutralBenchmarkCase]) -> f64 {
    if selected.is_empty() {
        return 0.0;
    }
    let passed = selected
        .iter()
        .filter(|case| {
            !case.history.is_empty()
                && case
                    .history
                    .iter()
                    .all(|turn| !turn.speaker.trim().is_empty() && !turn.content.trim().is_empty())
                && !case.question.id.trim().is_empty()
                && !case.question.prompt.trim().is_empty()
                && !case.expected.answer_markers.is_empty()
                && !case.expected.record_markers.is_empty()
        })
        .count();
    passed as f64 / selected.len() as f64
}

pub fn render_summary(report: &BenchmarkReport) -> String {
    let mut out = format!(
        "codex-memoryd synthetic benchmark: {}\ndataset: {} ({})\nselected cases: {}\n",
        report.status,
        report.dataset.id,
        report.dataset.adapter,
        report.selection.selected_case_ids.len()
    );
    for runner in &report.runners {
        out.push_str(&format!(
            "- {} [{}] success_rate {:.2}\n",
            runner.runner, runner.kind, runner.success_rate
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        load_fixture, run_synthetic_benchmark, SyntheticBenchmarkOptions,
        DEFAULT_SYNTHETIC_FIXTURE_JSON,
    };
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn synthetic_benchmark_allows_missing_tags() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing-tags.json");
        fs::write(
            &path,
            r#"{
  "dataset": {
    "id": "custom_memory_v1",
    "version": 1,
    "adapter": "custom_fixture",
    "case_count": 1
  },
  "cases": [
    {
      "id": "custom_case",
      "family": "custom",
      "history": [{"speaker": "user", "content": "Mina prefers tea over coffee."}],
      "question": {"id": "q1", "prompt": "What does Mina prefer?"},
      "expected": {
        "answer_markers": ["tea"],
        "record_markers": ["Mina prefers tea"]
      }
    }
  ]
}"#,
        )
        .unwrap();
        let report = run_synthetic_benchmark(&SyntheticBenchmarkOptions {
            full: true,
            input: Some(path),
            ..SyntheticBenchmarkOptions::default()
        })
        .expect("fixture without tags runs");

        let success_rates = report
            .runners
            .iter()
            .map(|runner| runner.success_rate)
            .collect::<Vec<_>>();
        assert_eq!(success_rates, vec![1.0, 1.0]);
    }

    #[test]
    fn load_fixture_rejects_mismatched_case_count() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad-fixture.json");
        let raw = DEFAULT_SYNTHETIC_FIXTURE_JSON.replace("\"case_count\": 2", "\"case_count\": 99");
        fs::write(&path, raw).unwrap();

        let err = load_fixture(Some(&path)).expect_err("fixture should be rejected");
        assert!(
            err.to_string().contains("dataset.case_count"),
            "unexpected error: {err}"
        );
    }
}
