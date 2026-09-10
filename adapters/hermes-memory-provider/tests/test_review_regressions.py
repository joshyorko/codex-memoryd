import json
from test_provider import CodexMemoryDProvider, MemoryDHandler, server


def test_bootstrap_is_once_per_destination_and_retries_failed_posts(tmp_path):
    httpd = server()
    try:
        (tmp_path / 'memories').mkdir()
        memory = tmp_path / 'memories/MEMORY.md'
        memory.write_bytes(b'  Synthetic origin\r\n')
        config = {'endpoint': f'http://127.0.0.1:{httpd.server_port}', 'bootstrap_origin': True}
        for text in [b'  Synthetic origin\r\n', b'Later memory must not be reimported']:
            memory.write_bytes(text)
            CodexMemoryDProvider(config).initialize('a', hermes_home=str(tmp_path))
        posts = [p for path,p in MemoryDHandler.requests if path == '/v1/conclusions']
        assert len(posts) == 1
        assert posts[0]['conclusions'] == ['  Synthetic origin\r\n']
        assert 'preserve_exact' not in posts[0]['metadata']
        assert posts[0]['metadata']['origin_digest'].startswith('sha256:')
    finally:
        httpd.shutdown(); httpd.server_close()


def test_bootstrap_resolves_active_home_when_keyword_is_omitted(tmp_path, monkeypatch):
    home = tmp_path / 'active-profile'
    (home / 'memories').mkdir(parents=True)
    (home / 'memories/MEMORY.md').write_text('Synthetic environment-resolved origin')
    monkeypatch.setenv('HERMES_HOME', str(home))
    httpd = server()
    try:
        provider = CodexMemoryDProvider({'endpoint': f'http://127.0.0.1:{httpd.server_port}'})
        provider.initialize('environment-home')
        posts = [payload for path, payload in MemoryDHandler.requests if path == '/v1/conclusions']
        assert len(posts) == 1
        assert posts[0]['conclusions'] == ['Synthetic environment-resolved origin']
        provider.initialize('second-session')
        assert len(MemoryDHandler.requests) == 1
    finally:
        httpd.shutdown()
        httpd.server_close()


def test_recall_renders_protocol_provenance():
    httpd = server()
    try:
        MemoryDHandler.recall_facts = [{'id': 'record-1', 'content': 'Synthetic evidence', 'policy': {'provenance': {'profile_id': 'personal', 'workspace_id': 'josh-personal', 'evidence_refs': ['source-1'], 'trust_level': 'medium'}}}]
        provider = CodexMemoryDProvider({'endpoint': f'http://127.0.0.1:{httpd.server_port}'})
        rendered = provider.prefetch('synthetic')
        assert 'source-1' in rendered
        assert 'josh-personal' in rendered
        assert 'record-1' in rendered
    finally:
        MemoryDHandler.recall_facts = []
        httpd.shutdown(); httpd.server_close()


def test_recall_label_injection_and_top_level_actor_fallback():
    httpd = server()
    try:
        MemoryDHandler.recall_facts = [
            {'id': 'record-1', 'content': 'safe\ncontent', 'policy': {'provenance': {'actor': 'bad]actor\nnext', 'target': 'user'}}},
            {'id': 'record-2', 'actor': 'agent:top', 'content': 'fallback content', 'policy': {'provenance': {'target': 'assistant'}}},
        ]
        provider = CodexMemoryDProvider({'endpoint': f'http://127.0.0.1:{httpd.server_port}'})
        rendered = provider.prefetch('synthetic')
        assert 'bad actor next' in rendered
        assert '\nnext]' not in rendered
        assert 'actor: agent:top' in rendered
    finally:
        MemoryDHandler.recall_facts = []
        httpd.shutdown(); httpd.server_close()
