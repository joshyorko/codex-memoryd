"""Real HTTP regressions for bounded, lane-local recall failures."""
import json
import threading
import time
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from test_provider import CodexMemoryDProvider


@contextmanager
def lane_server(slow_lane):
    calls = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            lane = payload['metadata']['lane']
            calls.append(lane)
            if lane == slow_lane:
                time.sleep(.7)
            raw = json.dumps({'ok': True, 'data': {'facts': [{
                'id': 'synthetic-' + lane, 'content': 'Synthetic fact ' + lane,
                'policy': {'provenance': {'workspace_id': payload['workspace'],
                                          'source_kind': 'synthetic-fixture'}},
            }]}}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            try:
                self.wfile.write(raw)
            except OSError:
                pass

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        try:
            yield f'http://127.0.0.1:{server.server_port}', calls
        finally:
            server.shutdown()
            worker.join()


def test_slow_lane_cannot_spend_later_lanes_time_budget():
    with lane_server('self') as (endpoint, calls):
        provider = CodexMemoryDProvider({'endpoint': endpoint, 'timeout_seconds': .4})
        started = time.monotonic()
        rendered = provider.prefetch('synthetic')
        elapsed = time.monotonic() - started
        assert calls == ['josh', 'self', 'relationship', 'evidence']
        assert '[ABOUT JOSH' in rendered
        assert '[SHARED HISTORY' in rendered
        assert '[CURRENT WORK' in rendered
        assert '[FRIDAY SELF' not in rendered
        assert 'workspace_id: friday-evidence' in rendered
        assert 'source_kind: synthetic-fixture' in rendered
        assert 'recall_not_authority' in rendered
        assert provider.recall_status().count == 3
        assert elapsed < .55


def test_partial_recall_reports_degradation_not_daemon_outage(caplog):
    with lane_server('evidence') as (endpoint, _):
        provider = CodexMemoryDProvider({'endpoint': endpoint, 'timeout_seconds': .2})
        assert provider.prefetch('private query sentinel')
        assert 'recall partial' in caplog.text
        assert 'completed=3 failed=1 recalled=3' in caplog.text
        assert 'unavailable' not in caplog.text
        assert 'private query sentinel' not in caplog.text
        assert 'Synthetic fact' not in caplog.text


def test_protocol_failure_diagnostics_do_not_log_server_text(caplog):
    import logging
    from test_transport import raw_server

    with caplog.at_level(logging.DEBUG), raw_server(b'private-response-sentinel\r\n\r\n') as endpoint:
        provider = CodexMemoryDProvider({'endpoint': endpoint})
        assert provider.prefetch('private-query-sentinel') == ''
        provider.sync_turn('private-user-sentinel', 'private-assistant-sentinel')
    assert 'recall failed: completed=0 failed=4 recalled=0' in caplog.text
    assert '/v1/recall BadStatusLine' in caplog.text
    assert '/v1/turns BadStatusLine' in caplog.text
    assert 'sentinel' not in caplog.text


def test_successful_empty_recall_is_not_reported_as_failure(caplog):
    from test_transport import raw_server

    raw = b'{"ok":true,"data":{"facts":[]}}'
    response = b'HTTP/1.1 200 OK\r\nContent-Length: ' + str(len(raw)).encode() + b'\r\n\r\n' + raw
    with raw_server(response) as endpoint:
        provider = CodexMemoryDProvider({'endpoint': endpoint})
        assert provider.prefetch('synthetic') == ''
        assert provider.recall_status() is None
    assert not caplog.records


def test_fast_lanes_donate_time_to_last_lane():
    from test_provider import provider_module
    from unittest.mock import patch

    clock = [0.0]
    budgets = []
    provider = CodexMemoryDProvider({'timeout_seconds': .5})

    def request(method, path, payload, timeout):
        budgets.append(timeout)
        clock[0] += .01
        return 200, b'{"ok":true,"data":{"facts":[]}}'

    with patch.object(provider_module.time, 'monotonic', lambda: clock[0]), patch.object(provider, '_request', request):
        assert provider.prefetch('synthetic') == ''
    assert len(budgets) == 4
    assert budgets[0] == .125
    assert budgets[-1] > .46
