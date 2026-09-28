use std::fs;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|err| panic!("{path}: {err}"))
}

#[test]
fn omp_adapter_is_native_registration_only_and_read_only_by_default() {
    let package = root().join("adapters/omp-memory-provider");
    for path in [
        "package.json",
        "README.md",
        "src/index.ts",
        "src/backend.ts",
        "src/client.ts",
        "src/config.ts",
        "src/format.ts",
        "src/guards.ts",
        "src/types.ts",
        "tests/adapter.test.ts",
    ] {
        assert!(package.join(path).exists(), "missing OMP adapter file: {path}");
    }

    let index = read("adapters/omp-memory-provider/src/index.ts");
    let readme = read("adapters/omp-memory-provider/README.md");
    let backend = read("adapters/omp-memory-provider/src/backend.ts");
    let client = read("adapters/omp-memory-provider/src/client.ts");
    assert!(index.contains("registerMemoryBackend"));
    assert!(index.contains("codex-memoryd"));
    assert!(backend.contains("beforeAgentStartPrompt"));
    assert!(backend.contains("preCompactionContext"));
    assert!(backend.contains("explicitSave"));
    assert!(!backend.contains("/v1/turns"));
    assert!(client.contains("/v1/conclusions"));
    assert!(!client.contains("/v1/turns"));
    assert!(readme.contains("NEEDS_OMP_SEAM"));
    assert!(readme.contains("autoObserve: true"));
    assert!(readme.contains("recall_not_authority"));
}

#[test]
fn omp_adapter_documents_transport_and_scope_bounds() {
    let readme = read("adapters/omp-memory-provider/README.md");
    let config = read("adapters/omp-memory-provider/src/config.ts");
    let client = read("adapters/omp-memory-provider/src/client.ts");
    assert!(readme.contains("500 ms"));
    assert!(readme.contains("4 MiB"));
    assert!(config.contains("loopback"));
    assert!(config.contains("profile") && config.contains("workspace"));
    assert!(client.contains("redirect: \"error\""));
    assert!(client.contains("readBoundedBody"));
}
