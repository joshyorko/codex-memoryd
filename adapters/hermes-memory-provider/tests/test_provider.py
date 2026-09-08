import importlib.util
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


MODULE = Path(__file__).parents[1] / "plugins" / "memory" / "codex_memoryd" / "__init__.py"
spec = importlib.util.spec_from_file_location("codex_memoryd_provider", MODULE)
provider_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provider_module)
CodexMemoryDProvider = provider_module.CodexMemoryDProvider


class MemoryDHandler(BaseHTTPRequestHandler):
    requests = []
    recall_facts = []

    def do_POST(self):
        length = int(self.headers["Content-Length"])
        payload = json.loads(self.rfile.read(length))
        self.requests.append((self.path, payload))
        if self.path == "/v1/recall":
            body = {"ok": True, "data": {
                "facts": self.recall_facts, "checkpoints": [], "citations": [],
                "summary": None, "withheld": [], "truncated": False,
                "authority": "recall_not_authority", "policy": {}, "pack": {},
            }, "warnings": []}
        elif self.path == "/v1/conclusions":
            body = {"ok": True, "data": {"created": ["synthetic-conclusion"], "record_ids": ["synthetic-record"], "rejected": 0}}
        else:
            body = {"ok": True, "data": {"accepted": 1, "rejected": 0,
                    "rejections": [], "source_ids": [], "derived_record_ids": []},
                    "warnings": []}
        raw = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def log_message(self, *_):
        pass


def server():
    MemoryDHandler.requests = []
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), MemoryDHandler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd


def test_prefetch_separates_lanes_and_preserves_memoryd_authority():
    httpd = server()
    MemoryDHandler.recall_facts = [
        {"id": "m1", "type": "identity", "scope": "workspace",
         "content": "FRIDAY origin record", "confidence": 1.0,
         "repo_id": None, "related_files": [], "updated_at": "now",
         "stale": False, "policy": {"provenance": {"source_kind": "hermes_builtin_memory_import"}}}
    ]
    provider = CodexMemoryDProvider({"endpoint": f"http://127.0.0.1:{httpd.server_port}",
                                     "profile": "personal", "workspace": "friday-self"})
    provider.initialize("session-1", agent_identity="friday", platform="cli")
    rendered = provider.prefetch("origin", session_id="session-1")
    request = MemoryDHandler.requests[0][1]
    assert "[FRIDAY SELF" in rendered
    assert "recall_not_authority" in rendered
    assert request["profile"] == "personal"
    workspaces = {payload["workspace"] for _, payload in MemoryDHandler.requests}
    assert {"josh-personal", "friday-self", "josh-friday", "friday-evidence"} <= workspaces
    assert request["metadata"]["agent"] == "agent:friday"
    httpd.shutdown()


def test_builtin_memory_write_is_mirrored_with_explicit_provenance():
    httpd = server()
    provider = CodexMemoryDProvider({"endpoint": f"http://127.0.0.1:{httpd.server_port}",
                                     "profile": "personal", "workspace": "friday-self"})
    provider.initialize("session-1", agent_identity="friday", platform="cli")
    provider.on_memory_write("add", "memory", "FRIDAY prefers evidence-backed changes.",
                             {"session_id": "session-1", "write_origin": "memory_tool"})
    path, request = MemoryDHandler.requests[0]
    assert path == "/v1/conclusions"
    assert request["target"] == "assistant"
    assert request["type"] == "workflow_pattern"
    assert request["metadata"]["actor"] == "agent:friday"
    assert request["metadata"]["source_kind"] == "friday_self_memory"
    httpd.shutdown()


def test_origin_bootstrap_sends_builtin_record_without_rewriting_it(tmp_path):
    httpd = server()
    origin = "FRIDAY origin record — source: friday-origin; activated_at: 2026-08-31T14:20:32Z"
    memory_dir = tmp_path / "memories"
    memory_dir.mkdir()
    (memory_dir / "MEMORY.md").write_text(origin, encoding="utf-8")
    provider = CodexMemoryDProvider({"endpoint": f"http://127.0.0.1:{httpd.server_port}"})
    provider.initialize("session-1", hermes_home=str(tmp_path), agent_identity="friday")
    path, request = MemoryDHandler.requests[0]
    assert path == "/v1/conclusions"
    assert request["conclusions"] == [origin]
    assert request["metadata"]["source_kind"] == "hermes_builtin_memory_import"
    assert request["metadata"]["source"] == "friday-origin"
    assert request["metadata"]["content_semantics"] == "normalized_conclusion_not_exact_archive"
    assert "preserve_exact" not in request["metadata"]
    httpd.shutdown()


def test_provider_fails_open_when_memoryd_is_unavailable():
    provider = CodexMemoryDProvider({"endpoint": "http://127.0.0.1:1", "timeout_seconds": 0.05})
    provider.initialize("session-1", agent_identity="friday", platform="cli")
    assert provider.prefetch("anything") == ""
    provider.sync_turn("hello", "world", session_id="session-1")
    provider.shutdown()
