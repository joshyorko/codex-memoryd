import socketserver
import threading
import time
from contextlib import contextmanager

import pytest
from test_provider import CodexMemoryDProvider


@contextmanager
def raw_server(response):
    class Handler(socketserver.BaseRequestHandler):
        def handle(self):
            self.request.recv(65536)
            if callable(response):
                response(self.request)
            else:
                self.request.sendall(response)
    with socketserver.ThreadingTCPServer(('127.0.0.1', 0), Handler) as server:
        worker = threading.Thread(target=server.serve_forever)
        worker.start()
        try:
            yield 'http://127.0.0.1:' + str(server.server_address[1])
        finally:
            server.shutdown()
            worker.join()


@pytest.mark.parametrize('response', [
    b'HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{"ok":',
    b'NOT HTTP\r\n\r\n',
])
def test_protocol_errors_fail_open(response, tmp_path):
    with raw_server(response) as endpoint:
        provider = CodexMemoryDProvider({'endpoint': endpoint})
        assert provider.prefetch('anything') == ''
        provider.sync_turn('hello', 'world')
        (tmp_path / 'memories').mkdir()
        (tmp_path / 'memories/MEMORY.md').write_text('synthetic origin')
        provider.initialize('test', hermes_home=str(tmp_path))
        if response.startswith(b'NOT'):
            assert not provider.is_available()


@pytest.mark.parametrize('phase', ['headers', 'body'])
def test_prefetch_absolute_deadline_on_real_slow_drip(phase):
    def drip(sock):
        try:
            if phase == 'body':
                sock.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n')
            for _ in range(40):
                sock.sendall(b' ')
                time.sleep(.03)
        except OSError:
            pass
    before = set(threading.enumerate())
    with raw_server(drip) as endpoint:
        provider = CodexMemoryDProvider({'endpoint': endpoint, 'timeout_seconds': .15})
        for _ in range(3):
            start = time.monotonic()
            assert provider.prefetch('slow daemon') == ''
            assert time.monotonic() - start < .5
    assert set(threading.enumerate()) == before
