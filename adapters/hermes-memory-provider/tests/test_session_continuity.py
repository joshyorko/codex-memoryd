from test_provider import CodexMemoryDProvider, MemoryDHandler, server


def test_session_switch_rebinds_turns_and_builtin_writes():
    httpd = server()
    try:
        provider = CodexMemoryDProvider({"endpoint": f"http://127.0.0.1:{httpd.server_port}", "bootstrap_origin": False})
        provider.initialize("old")
        provider.on_session_switch("new", parent_session_id="old", reset=True)
        provider.sync_turn("visible question", "visible answer", messages=[{"role": "assistant", "reasoning": "must not persist"}])
        provider.on_memory_write("add", "user", "Synthetic continuity fixture")
        turns = [p for path, p in MemoryDHandler.requests if path == "/v1/turns"]
        conclusions = [p for path, p in MemoryDHandler.requests if path == "/v1/conclusions"]
        assert turns[0]["session"]["id"] == "new"
        assert conclusions[0]["metadata"]["session_id"] == "new"
        assert [m["content"] for m in turns[0]["messages"]] == ["visible question", "visible answer"]
    finally:
        httpd.shutdown()
        httpd.server_close()


def test_outage_is_reported_as_warning(caplog):
    import logging
    with caplog.at_level(logging.WARNING):
        provider = CodexMemoryDProvider({"endpoint": "http://127.0.0.1:1", "bootstrap_origin": False, "timeout_seconds": 0.05})
        assert provider.prefetch("synthetic fixture") == ""
    assert "codex-memoryd recall failed: completed=0 failed=4 recalled=0" in caplog.text
