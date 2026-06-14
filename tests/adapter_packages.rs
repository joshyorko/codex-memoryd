use std::fs;
use std::path::Path;

const PACKAGES: &[&str] = &[
    "codex-mcp",
    "claude-local",
    "copilot-instructions",
    "generic-mcp-markdown",
];

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.as_ref().display()))
}

#[test]
fn adapter_packages_have_install_verify_uninstall_docs_and_env() {
    let adapters_dir = root().join("adapters");
    assert!(
        adapters_dir.join("README.md").exists(),
        "adapter package index is required"
    );

    for package in PACKAGES {
        let package_dir = adapters_dir.join(package);
        let readme = read(package_dir.join("README.md"));
        let env = read(package_dir.join(".env.example"));

        for heading in ["## Install", "## Verify", "## Uninstall"] {
            assert!(
                readme.contains(heading),
                "{package} README must include {heading}"
            );
        }
        for key in [
            "CODEX_MEMORYD_BIN=",
            "CODEX_MEMORYD_DB=",
            "CODEX_MEMORYD_PROFILE=",
            "CODEX_MEMORYD_WORKSPACE=",
            "CODEX_MEMORYD_BASE_URL=",
            "CODEX_MEMORYD_READ_ONLY=true",
        ] {
            assert!(env.contains(key), "{package} .env.example missing {key}");
        }
        assert!(
            !readme.contains("host-specific database"),
            "{package} must not introduce a host-specific memory database"
        );
    }
}

#[test]
fn mcp_adapter_templates_are_read_only_and_tool_limited() {
    let templates = [
        root().join("adapters/codex-mcp/templates/config.toml"),
        root().join("adapters/claude-local/templates/mcp-server.json"),
        root().join("adapters/generic-mcp-markdown/templates/mcp.json"),
    ];

    for path in templates {
        let body = read(&path);
        assert!(
            body.contains("--read-only"),
            "{} must invoke mcp stdio in read-only mode",
            path.display()
        );
        for tool in ["memory_status", "memory_recall", "memory_search"] {
            assert!(body.contains(tool), "{} must expose {tool}", path.display());
        }
        for write_tool in ["memory_conclude", "memory_checkpoint", "memory_dream"] {
            assert!(
                !body.contains(write_tool),
                "{} must not expose write tool {write_tool}",
                path.display()
            );
        }
    }
}

#[test]
fn markdown_adapter_templates_state_recall_not_authority() {
    let templates = [
        root().join("adapters/copilot-instructions/templates/copilot-instructions.md"),
        root().join("adapters/generic-mcp-markdown/templates/AGENTS.memory.md"),
        root().join("adapters/generic-mcp-markdown/templates/GEMINI.memory.md"),
    ];

    for path in templates {
        let body = read(&path);
        assert!(
            body.contains("recall_not_authority"),
            "{} must state recall-not-authority",
            path.display()
        );
        assert!(
            body.contains("read-only"),
            "{} must state read-only posture",
            path.display()
        );
        assert!(
            body.contains("codex-memoryd adapter export"),
            "{} must point at shared adapter export logic",
            path.display()
        );
    }
}
