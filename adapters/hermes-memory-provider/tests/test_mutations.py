import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from agent.memory_manager import MemoryManager
from test_provider import CodexMemoryDProvider


def test_native_remove_replace_archive_only_owned_ids_across_sessions(tmp_path):
    requests, active = [], {}
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            requests.append((self.path, payload))
            if self.path == '/v1/conclusions':
                text = payload['conclusions'][0]
                ids = []
                if text not in active.values():
                    id_ = 'record-' + str(len(requests))
                    active[id_] = text
                    ids = [id_]
                data = {'created': ['conclusion'], 'record_ids': ids}
            elif self.path == '/v1/forget':
                for id_ in payload['ids']:
                    active.pop(id_, None)
                data = {'archived': payload['ids'], 'not_found': [], 'errors': []}
            raw = json.dumps({'ok': True, 'data': data}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)
        def log_message(self, *_): pass
    with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            config = {'endpoint': f'http://127.0.0.1:{server.server_port}', 'bootstrap_origin': False}
            def manager(config=config):
                provider = CodexMemoryDProvider(config)
                provider.initialize('test', hermes_home=str(tmp_path))
                m = MemoryManager()
                m.add_provider(provider)
                return m
            def notify(m, action, **args):
                m.notify_memory_tool_write({'success': True}, {'target': 'user', 'action': action, **args})
            m = manager()
            active['unrelated'] = 'unrelated memory'
            notify(m, 'add', content='Uses violet lanterns for synthetic tests.')
            old_id = next(i for i in active if i != 'unrelated')
            m = manager()
            notify(m, 'replace', old_text='violet lanterns', new_text='Uses violet lanterns for synthetic tests.')
            assert old_id in active
            notify(m, 'replace', old_text='violet lanterns', new_text='Uses amber lanterns for synthetic tests.')
            assert old_id not in active
            assert 'Uses amber lanterns for synthetic tests.' in active.values()
            # Different destination cannot adopt or archive this mapping.
            notify(manager({**config, 'profile': 'work'}), 'remove', old_text='amber lanterns')
            assert 'Uses amber lanterns for synthetic tests.' in active.values()
            notify(m, 'remove', old_text='amber lanterns')
            assert active == {'unrelated': 'unrelated memory'}
            # A daemon-deduplicated record is not owned by this mirror.
            notify(m, 'add', content='unrelated memory')
            notify(m, 'remove', old_text='unrelated memory')
            assert active == {'unrelated': 'unrelated memory'}
            archives = [p for path, p in requests if path == '/v1/forget']
            assert len(archives) == 2
            assert all(p['mode'] == 'archive' and p['workspace'] == 'josh-personal' and p['profile'] == 'personal' for p in archives)
        finally:
            server.shutdown()
            thread.join()
