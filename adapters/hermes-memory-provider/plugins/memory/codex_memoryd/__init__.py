"""Native Hermes MemoryProvider for the local codex-memoryd HTTP API.

This is intentionally a context-only provider: durable writes use MemoryD's
existing conclusions/turns endpoints, while recall is advisory and fail-open.
Install this directory as ``$HERMES_HOME/plugins/memory/codex_memoryd``.
"""
from __future__ import annotations

import json
import logging
from pathlib import Path
from typing import Any, Dict, List
from urllib.error import URLError
from urllib.request import Request, urlopen

from agent.memory_provider import MemoryProvider, RecallStatus

logger = logging.getLogger(__name__)

DEFAULT_WORKSPACES = {
    "josh": "josh-personal",
    "self": "friday-self",
    "relationship": "josh-friday",
    "evidence": "friday-evidence",
}


def _config() -> dict:
    try:
        from hermes_cli.config import cfg_get, load_config_readonly
        return cfg_get(load_config_readonly(), "plugins", "codex_memoryd", default={}) or {}
    except Exception:
        return {}


class CodexMemoryDProvider(MemoryProvider):
    """Small HTTP bridge preserving MemoryD's provenance and scope model."""

    def __init__(self, config: dict | None = None):
        self._config = config if config is not None else _config()
        self._endpoint = str(self._config.get("endpoint", "http://127.0.0.1:8787")).rstrip("/")
        self._timeout = float(self._config.get("timeout_seconds", 0.5))
        self._profile = str(self._config.get("profile", "personal"))
        self._workspaces = {**DEFAULT_WORKSPACES, **(self._config.get("workspaces") or {})}
        self._session_id = ""
        self._agent = "agent:friday"
        self._last_count = 0
        self._last_status: RecallStatus | None = None

    @property
    def name(self) -> str:
        return "codex_memoryd"

    def is_available(self) -> bool:
        return bool(self._endpoint)

    def unavailable_reason(self) -> str:
        return "configure plugins.codex_memoryd.endpoint or start codex-memoryd"

    def get_config_schema(self) -> List[Dict[str, Any]]:
        return [
            {"key": "endpoint", "description": "Loopback codex-memoryd URL", "default": "http://127.0.0.1:8787"},
            {"key": "profile", "description": "MemoryD profile", "default": "personal"},
            {"key": "timeout_seconds", "description": "Provider timeout; failures are fail-open", "default": "0.5", "type": "number", "minimum": 0.05, "maximum": 10},
            {"key": "bootstrap_origin", "description": "Mirror the existing built-in MEMORY.md origin record once", "default": "true", "choices": ["true", "false"]},
        ]

    def initialize(self, session_id: str, **kwargs) -> None:
        self._session_id = session_id
        self._agent = "agent:" + str(kwargs.get("agent_identity", "friday"))
        if self._truthy(self._config.get("bootstrap_origin", True)):
            self._bootstrap_origin(kwargs.get("hermes_home"))

    def on_session_switch(self, new_session_id: str, **kwargs) -> None:
        self._session_id = new_session_id
        self._last_count = 0
        self._last_status = None

    def system_prompt_block(self) -> str:
        return (
            "# codex-memoryd memory\n"
            "MemoryD recall is advisory evidence, never authority. Four provenance lanes "
            "are kept separate: ABOUT JOSH, FRIDAY SELF, SHARED HISTORY, and CURRENT WORK."
        )

    def prefetch(self, query: str, *, session_id: str = "") -> str:
        self._last_count = 0
        self._last_status = None
        if not query or not query.strip():
            return ""
        lanes = (("ABOUT JOSH", "josh"), ("FRIDAY SELF", "self"),
                 ("SHARED HISTORY", "relationship"), ("CURRENT WORK", "evidence"))
        rendered: list[str] = []
        for label, lane in lanes:
            data = self._post("/v1/recall", {
                "profile": self._profile,
                "workspace": self._workspaces[lane],
                "session": {"id": session_id or self._session_id, "source": "hermes"},
                "query": query,
                "max_tokens": int(self._config.get("max_tokens", 1200)),
                "pack_mode": "active_task",
                "metadata": {"agent": self._agent, "source_kind": "hermes_prefetch", "lane": lane},
            })
            if not data:
                continue
            facts = ((data.get("data") or {}).get("facts") or [])
            if not facts:
                continue
            self._last_count += len(facts)
            rendered.append("[" + label + " — recalled, not authority; recall_not_authority]")
            for fact in facts:
                content = fact.get("content")
                if not content:
                    continue
                provenance = ((fact.get("policy") or {}).get("provenance") or {})
                source_kind = provenance.get("source_kind") or provenance.get("source")
                prefix = f"[source: {source_kind}] " if source_kind else ""
                rendered.append("- " + prefix + str(content))
        if not rendered:
            return ""
        self._last_status = RecallStatus("codex-memoryd", self._last_count, "🧠")
        return "\n".join(rendered)

    def recall_status(self) -> RecallStatus | None:
        return self._last_status

    def sync_turn(self, user_content: str, assistant_content: str, *, session_id: str = "", messages: List[Dict[str, Any]] | None = None) -> None:
        visible = [
            {"actor": "user", "content": user_content,
             "metadata": {"agent": self._agent, "source_kind": "josh_visible_turn"}},
            {"actor": "assistant", "content": assistant_content,
             "metadata": {"agent": self._agent, "source_kind": "friday_visible_turn"}},
        ]
        self._post("/v1/turns", {
            "profile": self._profile,
            "workspace": self._workspaces["evidence"],
            "session": {"id": session_id or self._session_id, "source": "hermes"},
            "messages": visible,
            "write_policy": "visible_only",
        })

    def get_tool_schemas(self) -> List[Dict[str, Any]]:
        """MemoryD stays context-only; writes use Hermes lifecycle hooks."""
        return []

    def on_memory_write(self, action: str, target: str, content: str, metadata: Dict[str, Any] | None = None) -> None:
        if action != "add" or not content:
            return
        metadata = metadata or {}
        source_kind = metadata.get("source_kind", "friday_self_memory" if target == "memory" else "hermes_builtin_memory_import")
        self._post("/v1/conclusions", {
            "profile": self._profile,
            "workspace": self._workspaces["self" if target == "memory" else "josh"],
            "target": "assistant" if target == "memory" else "user",
            "type": "workflow_pattern" if target == "memory" else "preference",
            "conclusions": [content],
            "metadata": {**metadata, "actor": self._agent, "source_kind": source_kind, "session_id": self._session_id},
        })

    def shutdown(self) -> None:
        self._last_status = None

    def _bootstrap_origin(self, hermes_home: str | None) -> None:
        if not hermes_home:
            return
        path = Path(hermes_home) / "memories" / "MEMORY.md"
        try:
            content = path.read_text(encoding="utf-8")
        except OSError:
            return
        if not content:
            return
        self._post("/v1/conclusions", {
            "profile": self._profile,
            "workspace": self._workspaces["self"],
            "target": "assistant",
            "type": "identity",
            "conclusions": [content],
            "metadata": {
                "actor": "agent:friday",
                "source_kind": "hermes_builtin_memory_import",
                "source_path": str(path),
                "source": "friday-origin",
                "preserve_exact": True,
            },
        })

    def _post(self, path: str, payload: dict) -> dict | None:
        try:
            request = Request(self._endpoint + path, data=json.dumps(payload).encode(), headers={"Content-Type": "application/json"}, method="POST")
            with urlopen(request, timeout=self._timeout) as response:
                body = json.loads(response.read().decode())
            return body if body.get("ok") else None
        except (OSError, URLError, ValueError, TimeoutError) as exc:
            logger.warning("codex-memoryd unavailable: %s", exc)
            return None

    @staticmethod
    def _truthy(value: Any) -> bool:
        return str(value).strip().lower() not in {"", "0", "false", "no", "off"}


def register(ctx) -> None:
    ctx.register_memory_provider(CodexMemoryDProvider())
