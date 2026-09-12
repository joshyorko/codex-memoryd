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
