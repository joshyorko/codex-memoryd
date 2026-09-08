"""Native Hermes MemoryProvider for the local codex-memoryd HTTP API.

This is intentionally a context-only provider: durable writes use MemoryD's
existing conclusions/turns endpoints, while recall is advisory and fail-open.
Install this directory as ``$HERMES_HOME/plugins/codex_memoryd``.
"""
from __future__ import annotations

import json
import hashlib
import sqlite3
import time
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
        self._health_checked_at = float("-inf")
        self._healthy = False

    @property
    def name(self) -> str:
        return "codex_memoryd"

    def is_available(self) -> bool:
        if not self._endpoint:
            return False
        now = time.monotonic()
        if now - self._health_checked_at < 2.0:
            return self._healthy
        try:
            with urlopen(self._endpoint + "/healthz", timeout=min(self._timeout, 0.5)) as response:
                self._healthy = response.status == 200
        except (OSError, URLError, ValueError, TimeoutError):
            self._healthy = False
        self._health_checked_at = time.monotonic()
        return self._healthy

    def unavailable_reason(self) -> str:
        return "codex-memoryd unavailable; check plugins.codex_memoryd.endpoint and daemon health"

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
        deadline = time.monotonic() + self._timeout
        for label, lane in lanes:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            data = self._post("/v1/recall", {
                "profile": self._profile,
                "workspace": self._workspaces[lane],
                "session": {"id": session_id or self._session_id, "source": "hermes"},
                "query": query,
                "max_tokens": int(self._config.get("max_tokens", 1200)),
                "pack_mode": "active_task",
                "metadata": {"agent": self._agent, "source_kind": "hermes_prefetch", "lane": lane},
            }, timeout=remaining)
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
                labels = [f"record: {fact['id']}"] if fact.get("id") else []
                for key in ("profile_id", "workspace_id", "trust_level"):
                    if provenance.get(key):
                        labels.append(f"{key}: {provenance[key]}")
                refs = list(provenance.get("evidence_refs") or [])
                refs.extend(c["source_id"] for c in (data.get("data") or {}).get("citations", [])
                            if c.get("memory_id") == fact.get("id") and c.get("source_id"))
                if refs:
                    labels.append("evidence: " + ", ".join(dict.fromkeys(refs)))
                prefix = "[" + "; ".join(labels) + "] " if labels else ""
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
            raw = path.read_bytes()
            content = raw.decode("utf-8")
            if not content.strip():
                return
            state_dir = Path(hermes_home) / "state" / "codex_memoryd"
            state_dir.mkdir(parents=True, exist_ok=True)
            destination = json.dumps([self._endpoint, self._profile, self._workspaces["self"]])
            digest = "sha256:" + hashlib.sha256(raw).hexdigest()
            # Serialize concurrent initializations; store only an acknowledgement,
            # never the private memory text. A changed MEMORY.md is not a new origin.
            with sqlite3.connect(state_dir / "bootstrap.sqlite3", timeout=0.1) as db:
                db.execute("CREATE TABLE IF NOT EXISTS imports (destination TEXT PRIMARY KEY, digest TEXT NOT NULL)")
                db.execute("BEGIN IMMEDIATE")
                if db.execute("SELECT 1 FROM imports WHERE destination = ?", (destination,)).fetchone():
                    return
                result = self._post("/v1/conclusions", {
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
                        "origin_id": "friday-origin",
                        "origin_digest": digest,
                        "content_semantics": "normalized_conclusion_not_exact_archive",
                    },
                })
                if result and (result.get("data") or {}).get("created"):
                    db.execute("INSERT INTO imports VALUES (?, ?)", (destination, digest))
        except (OSError, UnicodeError, sqlite3.Error) as exc:
            logger.warning("codex-memoryd bootstrap skipped: %s", exc)

    def _post(self, path: str, payload: dict, *, timeout: float | None = None) -> dict | None:
        try:
            request = Request(self._endpoint + path, data=json.dumps(payload).encode(), headers={"Content-Type": "application/json"}, method="POST")
            with urlopen(request, timeout=self._timeout if timeout is None else timeout) as response:
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
