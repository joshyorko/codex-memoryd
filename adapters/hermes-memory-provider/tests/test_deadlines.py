import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from test_provider import CodexMemoryDProvider


class SlowHandler(BaseHTTPRequestHandler):
    calls = 0
    def do_POST(self):
        type(self).calls += 1
        time.sleep(.3)
        self.send_response(200)
        self.end_headers()
        try:
            self.wfile.write(b'{"ok":true,"data":{"facts":[]}}')
        except BrokenPipeError:
            pass
    def do_GET(self):
        type(self).calls += 1
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b'{"ok":true}')
    def log_message(self, *_):
        pass


def test_prefetch_uses_one_deadline_for_all_lanes():
    SlowHandler.calls = 0
    server = ThreadingHTTPServer(('127.0.0.1', 0), SlowHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        p = CodexMemoryDProvider({'endpoint':f'http://127.0.0.1:{server.server_port}', 'timeout_seconds':.08})
        start = time.monotonic()
        assert p.prefetch('synthetic') == ''
        assert time.monotonic() - start < .2
        assert SlowHandler.calls == 4
    finally:
        server.shutdown(); server.server_close()


def test_availability_checks_health_and_caches_result():
    SlowHandler.calls = 0
    server = ThreadingHTTPServer(('127.0.0.1', 0), SlowHandler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    p = CodexMemoryDProvider({'endpoint':f'http://127.0.0.1:{server.server_port}', 'timeout_seconds':.08})
    try:
        assert p.is_available()
        assert p.is_available()
        assert SlowHandler.calls == 1
    finally:
        server.shutdown(); server.server_close()
    down = CodexMemoryDProvider({'endpoint':f'http://127.0.0.1:{server.server_port}', 'timeout_seconds':.08})
    assert not down.is_available()
    assert 'unavailable' in down.unavailable_reason()
