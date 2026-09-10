#!/usr/bin/env python3
"""Deterministic live Hermes-provider -> disposable MemoryD acceptance."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import tempfile
from http.client import HTTPConnection
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlsplit

from agent.inline_tool_executors import INLINE_TOOL_EXECUTORS, InlineToolContext
from agent.memory_manager import MemoryManager
from tools.memory_tool_store import MemoryStore


ROOT = Path(__file__).parents[1]
ADAPTER = ROOT / "adapters/hermes-memory-provider/plugins/memory/codex_memoryd/__init__.py"
OLD = "Synthetic correction: the violet route is current."
NEW = "Synthetic correction: the amber route is current."
QUERY = "Which correction should a fresh reader recall?"


def _load_provider_class():
    spec = importlib.util.spec_from_file_location("live_codex_memoryd_provider", ADAPTER)
    if spec is None or spec.loader is None:
        raise RuntimeError("could not load Hermes MemoryD adapter")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.CodexMemoryDProvider


CodexMemoryDProvider = _load_provider_class()


def _post(endpoint: str, path: str, payload: dict) -> dict:
    url = urlsplit(endpoint)
    connection = HTTPConnection(url.hostname, url.port, timeout=3)
    try:
        body = json.dumps(payload).encode("utf-8")
        connection.request(
            "POST",
            path,
            body=body,
            headers={"Content-Type": "application/json", "Content-Length": str(len(body))},
        )
        response = connection.getresponse()
        raw = response.read()
        result = json.loads(raw.decode("utf-8"))
        if response.status != 200 or not result.get("ok"):
            raise RuntimeError(f"MemoryD {path} returned HTTP {response.status}")
        return result
    finally:
        connection.close()


def _recall(endpoint: str, profile: str, workspace: str, query: str) -> dict:
    return _post(endpoint, "/v1/recall", {
        "profile": profile,
        "workspace": workspace,
        "query": query,
        "max_tokens": 1200,
        "pack_mode": "active_task",
    })["data"]


def _conclusion(endpoint: str, profile: str, workspace: str, content: str, metadata: dict) -> dict:
    return _post(endpoint, "/v1/conclusions", {
        "profile": profile,
        "workspace": workspace,
        "target": "assistant",
        "type": "workflow_pattern",
        "conclusions": [content],
        "metadata": metadata,
    })["data"]


def _native_context(endpoint: str, home: Path, profile: str = "personal"):
    previous_home = os.environ.get("HERMES_HOME")
    os.environ["HERMES_HOME"] = str(home)
    provider = CodexMemoryDProvider({
        "endpoint": endpoint,
        "profile": profile,
        "bootstrap_origin": False,
    })
    provider.initialize("writer-session", hermes_home=str(home), agent_identity="friday", platform="cli")
    manager = MemoryManager()
    manager.add_provider(provider)
    store = MemoryStore(memory_char_limit=500, user_char_limit=300)
    agent = SimpleNamespace(
        _memory_store=store,
        _memory_manager=manager,
        _build_memory_write_metadata=lambda **_: {
            "write_origin": "assistant_tool",
            "execution_context": "foreground",
            "session_id": "writer-session",
            "tool_name": "memory",
        },
    )
    context = InlineToolContext(effective_task_id="synthetic-task", tool_call_id="synthetic-call")

    def call(args: dict) -> dict:
        result = json.loads(INLINE_TOOL_EXECUTORS["memory"](agent, args, context))
        if result.get("success") is not True:
            raise RuntimeError(
                f"native Hermes memory operation was not committed: {result.get('error', 'unknown error')}"
            )
        return result

    def close() -> None:
        provider.shutdown()
        if previous_home is None:
            os.environ.pop("HERMES_HOME", None)
        else:
            os.environ["HERMES_HOME"] = previous_home

    return call, close


def _write_native(endpoint: str) -> None:
    with tempfile.TemporaryDirectory(prefix="hermes-memoryd-writer-") as raw_home:
        call, close = _native_context(endpoint, Path(raw_home))
        try:
            call({"action": "add", "target": "memory", "content": OLD})
            call({
                "action": "replace",
                "target": "memory",
                "old_text": "violet route",
                "content": NEW,
            })
            data = _recall(endpoint, "personal", "friday-self", "amber route")
            facts = data.get("facts", [])
            if not any(NEW in fact.get("content", "") for fact in facts):
                raise RuntimeError("native write did not reach the live MemoryD daemon")
            if any(OLD in fact.get("content", "") for fact in facts):
                raise RuntimeError("live daemon still recalls the archived correction")
        finally:
            close()


def _read_fresh(endpoint: str) -> dict:
    with tempfile.TemporaryDirectory(prefix="hermes-memoryd-reader-") as raw_home:
        reader_home = Path(raw_home)
        fresh_files_before = [path for path in reader_home.rglob("*") if path.is_file()]
        if fresh_files_before:
            raise RuntimeError("fresh reader home was not empty before provider recall")
        provider = CodexMemoryDProvider({
            "endpoint": endpoint,
            "profile": "personal",
            "bootstrap_origin": False,
        })
        provider.initialize("reader-session", hermes_home=str(reader_home), agent_identity="friday", platform="cli")
        try:
            rendered = provider.prefetch(QUERY, session_id="reader-session")
            without_provider = MemoryManager().prefetch_all(QUERY, session_id="reader-session")
        finally:
            provider.shutdown()
        result = {
            "mode": "read",
            "query_contains_answer": NEW in QUERY,
            "fresh_home_empty_before": not fresh_files_before,
            "with_provider_contains_answer": NEW in rendered,
            "with_provider_contains_old": OLD in rendered,
            "without_provider": without_provider,
            "with_provider_scope": "workspace_id: friday-self" in rendered,
            "with_provider_origin": "origin: conclusion" in rendered,
            "with_provider_source_kind": "source_kind: friday_self_memory" in rendered,
            "with_provider_actor": "actor: agent:friday" in rendered,
            "with_provider_write_origin": "write_origin: assistant_tool" in rendered,
            "with_provider_session": "session_id: writer-session" in rendered,
        }
        if result["query_contains_answer"] or not result["fresh_home_empty_before"]:
            raise RuntimeError("fresh consumer input already contained the expected answer")
        if not result["with_provider_contains_answer"] or result["with_provider_contains_old"]:
            raise RuntimeError("fresh scoped recall did not return only the corrected fact")
        if result["without_provider"]:
            raise RuntimeError("without-provider control unexpectedly returned context")
        if not all(result[key] for key in (
            "with_provider_scope",
            "with_provider_origin",
            "with_provider_source_kind",
            "with_provider_actor",
            "with_provider_write_origin",
            "with_provider_session",
        )):
            raise RuntimeError("fresh recall omitted native conclusion provenance")
        return result


def _controls(endpoint: str) -> dict:
    owned = "Synthetic ownership control: remove this record."
    with tempfile.TemporaryDirectory(prefix="hermes-memoryd-owned-") as raw_home:
        call, close = _native_context(endpoint, Path(raw_home))
        try:
            call({"action": "add", "target": "memory", "content": owned})
            call({"action": "remove", "target": "memory", "old_text": owned})
        finally:
            close()
    owned_facts = _recall(endpoint, "personal", "friday-self", "ownership control")
    owned_removed = not any(owned in fact.get("content", "") for fact in owned_facts.get("facts", []))

    unowned = "Synthetic ownership control: unrelated writer record."
    direct = _conclusion(endpoint, "personal", "friday-self", unowned, {
        "source_kind": "synthetic_unrelated_writer",
        "actor": "synthetic-writer",
    })
    if not direct.get("record_ids"):
        raise RuntimeError("unowned control record was not created")
    with tempfile.TemporaryDirectory(prefix="hermes-memoryd-dedup-") as raw_home:
        call, close = _native_context(endpoint, Path(raw_home))
        try:
            call({"action": "add", "target": "memory", "content": unowned})
            call({"action": "remove", "target": "memory", "old_text": "unrelated writer"})
        finally:
            close()
    unowned_facts = _recall(endpoint, "personal", "friday-self", "unrelated writer")
    unowned_survives = any(unowned in fact.get("content", "") for fact in unowned_facts.get("facts", []))

    scoped = "Synthetic scope control: personal record survives work removal."
    with tempfile.TemporaryDirectory(prefix="hermes-memoryd-scope-personal-") as personal_home:
        personal_call, personal_close = _native_context(endpoint, Path(personal_home), "personal")
        try:
            personal_call({"action": "add", "target": "memory", "content": scoped})
        finally:
            personal_close()
        with tempfile.TemporaryDirectory(prefix="hermes-memoryd-scope-work-") as work_home:
            work_provider = CodexMemoryDProvider({
                "endpoint": endpoint,
                "profile": "work",
                "bootstrap_origin": False,
            })
            work_provider.initialize(
                "work-session",
                hermes_home=str(work_home),
                agent_identity="friday",
                platform="cli",
            )
            try:
                work_provider.on_memory_write(
                    "remove",
                    "memory",
                    "",
                    {"old_text": scoped, "write_origin": "assistant_tool"},
                )
            finally:
                work_provider.shutdown()
    scoped_facts = _recall(endpoint, "personal", "friday-self", "personal record survives")
    scope_preserved = any(scoped in fact.get("content", "") for fact in scoped_facts.get("facts", []))

    result = {
        "mode": "controls",
        "owned_removed": owned_removed,
        "unowned_dedup_record_survives": unowned_survives,
        "cross_profile_scope_preserved": scope_preserved,
    }
    if not all(result.values()):
        raise RuntimeError("live ownership or scope control failed")
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--mode", choices=("write", "read", "controls"), required=True)
    args = parser.parse_args()
    try:
        if args.mode == "write":
            _write_native(args.endpoint)
            result = {"mode": "write", "native_add_replace": True}
        elif args.mode == "read":
            result = _read_fresh(args.endpoint)
        else:
            result = _controls(args.endpoint)
        print(json.dumps({"status": "PASS", **result}, sort_keys=True))
        return 0
    except Exception as exc:
        print(json.dumps({"status": "FAIL", "mode": args.mode, "error": str(exc)}, sort_keys=True))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
