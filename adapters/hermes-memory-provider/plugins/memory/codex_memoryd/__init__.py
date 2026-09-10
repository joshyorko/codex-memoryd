"""Native Hermes MemoryProvider for the local codex-memoryd HTTP API.

This is intentionally a context-only provider: durable writes use MemoryD's
existing conclusions/turns endpoints, while recall is advisory and fail-open.
Install this directory as ``$HERMES_HOME/plugins/codex_memoryd``.
"""
from __future__ import annotations

import json
from http.client import HTTPException, HTTPConnection, HTTPSConnection, HTTPResponse
import io
import ipaddress
import hashlib
import sqlite3
import time
import logging
from pathlib import Path
from typing import Any, Dict, List
from urllib.error import URLError
from urllib.parse import urlsplit

from agent.memory_provider import MemoryProvider, RecallStatus
from agent.model_metadata import estimate_tokens_rough

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


class _DeadlineReader(io.RawIOBase):
    """Re-arm the remaining wall-clock budget for every underlying recv."""

    def __init__(self, sock, deadline):
        self.sock = sock
        self.deadline = deadline

    def readable(self):
        return True

    def readinto(self, buffer):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("MemoryD response deadline exceeded")
        self.sock.settimeout(remaining)
        return self.sock.recv_into(buffer)


class _ResponseSocket:
    def __init__(self, sock, deadline):
        self.sock, self.deadline = sock, deadline

    def makefile(self, mode):
        return io.BufferedReader(_DeadlineReader(self.sock, self.deadline))


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
        self._hermes_home: Path | None = None

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
            status, _ = self._request("GET", "/healthz", None, min(self._timeout, 0.5))
            self._healthy = status == 200
        except (OSError, URLError, ValueError, TimeoutError, HTTPException):
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
        from hermes_constants import get_hermes_home
        self._hermes_home = Path(kwargs.get("hermes_home") or get_hermes_home())
        self._session_id = session_id
        self._agent = "agent:" + str(kwargs.get("agent_identity", "friday"))
        if self._truthy(self._config.get("bootstrap_origin", True)):
            self._bootstrap_origin(str(self._hermes_home))

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
        budget = max(0, int(self._config.get("max_tokens", 1200)))
        deadline = time.monotonic() + self._timeout
        for label, lane in lanes:
            remaining = deadline - time.monotonic()
            tokens_left = budget - estimate_tokens_rough("\n".join(rendered))
            if remaining <= 0 or tokens_left <= 0:
                break
            data = self._post("/v1/recall", {
                "profile": self._profile,
                "workspace": self._workspaces[lane],
                "session": {"id": session_id or self._session_id, "source": "hermes"},
                "query": query,
                "max_tokens": tokens_left,
                "pack_mode": "active_task",
                "metadata": {"agent": self._agent, "source_kind": "hermes_prefetch", "lane": lane},
            }, timeout=remaining)
            if not data:
                continue
            facts = ((data.get("data") or {}).get("facts") or [])
            if not facts:
                continue
            header = "[" + label + " — recalled, not authority; recall_not_authority]"
            lane_started = False
            for fact in facts:
                content = fact.get("content")
                if not content:
                    continue
                provenance = ((fact.get("policy") or {}).get("provenance") or {})
                labels = [f"record: {fact['id']}"] if fact.get("id") else []
                for key in ("profile_id", "workspace_id", "trust_level", "origin", "target",
                            "source_kind", "actor", "write_origin", "session_id"):
                    if provenance.get(key):
                        labels.append(f"{key}: {provenance[key]}")
                refs = list(provenance.get("evidence_refs") or [])
                refs.extend(c["source_id"] for c in (data.get("data") or {}).get("citations", [])
                            if c.get("memory_id") == fact.get("id") and c.get("source_id"))
                if refs:
                    labels.append("evidence: " + ", ".join(dict.fromkeys(refs)))
                prefix = "[" + "; ".join(labels) + "] " if labels else ""
                addition = ([] if lane_started else [header]) + ["- " + prefix + str(content)]
                if estimate_tokens_rough("\n".join(rendered + addition)) > budget:
                    continue
                rendered.extend(addition)
                lane_started = True
                self._last_count += 1
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
        if action not in {"add", "remove", "replace"} or target not in {"memory", "user"}:
            return
        if self._hermes_home is None:
            return
        metadata = metadata or {}
        content = content.strip()
        old_text = str(metadata.get("old_text") or "").strip()
        workspace = self._workspaces["self" if target == "memory" else "josh"]
        destination = json.dumps([self._endpoint, self._profile, workspace, target])
        try:
            state = self._hermes_home / "state" / "codex_memoryd"
            state.mkdir(parents=True, exist_ok=True)
            with sqlite3.connect(state / "mirrors.sqlite3", timeout=0.1) as db:
                db.execute("CREATE TABLE IF NOT EXISTS mirrors (destination TEXT, content TEXT, ids TEXT, PRIMARY KEY(destination, content))")
                db.execute("BEGIN IMMEDIATE")
                if action in {"remove", "replace"}:
                    if not old_text:
                        return
                    matches = [(text, ids) for text, ids in db.execute(
                        "SELECT content, ids FROM mirrors WHERE destination = ?", (destination,)) if old_text in text]
                    # Hermes matches a unique substring, not a record number.
                    # Never guess when older mappings are ambiguous or absent.
                    if len(matches) > 1:
                        logger.warning("codex-memoryd mirror mutation is ambiguous")
                        return
                    if matches:
                        text, encoded_ids = matches[0]
                        if action == "replace" and text == content:
                            return
                        ids = json.loads(encoded_ids)
                        if ids:
                            result = self._post("/v1/forget", {
                                "profile": self._profile, "workspace": workspace,
                                "ids": ids, "mode": "archive",
                            })
                            data = (result or {}).get("data") or {}
                            if not result or data.get("errors") or not set(ids) <= set(data.get("archived", []) + data.get("not_found", [])):
                                return
                        db.execute("DELETE FROM mirrors WHERE destination = ? AND content = ?", (destination, text))
                if action == "remove" or not content:
                    return
                if db.execute("SELECT 1 FROM mirrors WHERE destination = ? AND content = ?", (destination, content)).fetchone():
                    return
                result = self._write_conclusion(target, content, metadata)
                if result and (result.get("data") or {}).get("created"):
                    # Only newly created record_ids confer ownership. A dedup hit
                    # may belong to an origin import or another writer: never adopt it.
                    ids = (result.get("data") or {}).get("record_ids") or []
                    db.execute("INSERT INTO mirrors VALUES (?, ?, ?)", (destination, content, json.dumps(ids)))
        except (OSError, sqlite3.Error) as exc:
            logger.warning("codex-memoryd mirror skipped: %s", exc)

    def _write_conclusion(self, target, content, metadata):
        source_kind = metadata.get("source_kind", "friday_self_memory" if target == "memory" else "hermes_native_memory")
        return self._post("/v1/conclusions", {
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

    def _request(self, method, path, payload, timeout):
        deadline = time.monotonic() + timeout
        url = urlsplit(self._endpoint + path)
        # This is a local-daemon adapter. Avoid unbounded DNS lookup entirely;
        # localhost is a fixed loopback alias, otherwise require a numeric IP.
        host = "127.0.0.1" if url.hostname == "localhost" else str(ipaddress.ip_address(url.hostname))
        connection_type = {"http": HTTPConnection, "https": HTTPSConnection}.get(url.scheme)
        if connection_type is None:
            raise ValueError("MemoryD endpoint must use HTTP or HTTPS")
        connection = connection_type(host, url.port, timeout=timeout)
        try:
            connection.connect()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("MemoryD request deadline exceeded")
            connection.sock.settimeout(remaining)
            body = None if payload is None else json.dumps(payload).encode()
            connection.request(method, url.path, body=body, headers={"Content-Type": "application/json"})
            with HTTPResponse(_ResponseSocket(connection.sock, deadline), method=method) as response:
                response.begin()
                raw = response.read(4 * 1024 * 1024 + 1)
                if len(raw) > 4 * 1024 * 1024:
                    raise ValueError("MemoryD response exceeds size limit")
                if response.length not in (None, 0):
                    raise HTTPException("Truncated MemoryD response")
                if time.monotonic() >= deadline:
                    raise TimeoutError("MemoryD response deadline exceeded")
                return response.status, raw
        finally:
            connection.close()

    def _post(self, path: str, payload: dict, *, timeout: float | None = None) -> dict | None:
        try:
            status, raw = self._request("POST", path, payload, self._timeout if timeout is None else timeout)
            if status != 200:
                return None
            body = json.loads(raw.decode())
            return body if isinstance(body, dict) and body.get("ok") else None
        except (OSError, URLError, ValueError, TimeoutError, HTTPException) as exc:
            logger.warning("codex-memoryd unavailable: %s", exc)
            return None

    @staticmethod
    def _truthy(value: Any) -> bool:
        return str(value).strip().lower() not in {"", "0", "false", "no", "off"}


def register(ctx) -> None:
    ctx.register_memory_provider(CodexMemoryDProvider())
