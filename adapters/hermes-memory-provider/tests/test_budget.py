from agent.model_metadata import estimate_tokens_rough
from test_provider import CodexMemoryDProvider, MemoryDHandler, server


def test_one_total_budget_includes_lane_headers_and_provenance():
    httpd = server()
    try:
        MemoryDHandler.recall_facts = [
            {'id': 'record-' + str(i), 'content': '合成 evidence ' * 12,
             'policy': {'provenance': {'profile_id': 'personal', 'workspace_id': 'fixture',
                                      'evidence_refs': ['source-' + str(i)], 'trust_level': 'medium'}}}
            for i in range(3)
        ]
        provider = CodexMemoryDProvider({'endpoint': f'http://127.0.0.1:{httpd.server_port}', 'max_tokens': 300})
        rendered = provider.prefetch('synthetic')
        assert rendered
        assert estimate_tokens_rough(rendered) <= 300
        requests = [p for path, p in MemoryDHandler.requests if path == '/v1/recall']
        assert len(requests) > 1
        assert requests[1]['max_tokens'] < requests[0]['max_tokens']
        assert provider.recall_status().count == rendered.count('\n- ')
        provider._config['max_tokens'] = 1
        assert provider.prefetch('synthetic') == ''
        assert provider.recall_status() is None
    finally:
        MemoryDHandler.recall_facts = []
        httpd.shutdown()
        httpd.server_close()
