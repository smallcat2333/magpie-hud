"""Verify decision-route IPC/UI in an isolated native HUD with a loopback fake gateway."""
import argparse
import base64
import datetime as dt
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

from verify_routing_window import CDP

ROOT = Path(__file__).resolve().parents[1]
now = dt.datetime.now().astimezone().isoformat(timespec='milliseconds')
state = {'pid': 901, 'seq': 1, 'ids': [41], 'routes': [], 'active': []}
requests = {'trace': 0, 'concurrency': 0}
lock = threading.Lock()


def route():
    return {'id': 41, 'session': 'decision-fixture', 'agent': 'claude-desktop', 'time': now,
            'done': False, 'group': {'id': 'auto', 'name': '自动', 'routing': 'order',
                                      'subs': [{'id': 'worker', 'name': '干活', 'in': 'auto', 'routing': 'order'}]},
            'nested': [{'group': 'worker', 'name': '干活', 'rule': {'n': 0, 'turn': 7, 'held': True, 'pick': 'high'}}],
            'order': [{'id': 'one', 'provider': 'fixture', 'who': 'fixture@example.com', 'model': 'model-a', 'via': ['worker']},
                      {'id': 'two', 'provider': 'fixture', 'who': 'fixture@example.com', 'model': 'model-b', 'via': ['worker']}],
            'rule': {'n': 5, 'turn': 7, 'held': True, 'use': 'group/worker', 'pick': 'high',
                     'classified': {'by': 'test/classifier-v2', 'intent': '代码分析', 'sure': .87},
                     'when': ['PRIVATE-EXPRESSION']},
            'tries': [{'id': 'one', 'model': 'model-a', 'effort': 'high', 'done': True, 'status': 503},
                      {'id': 'two', 'model': 'model-b', 'effort': 'high', 'done': False}]}


class Gateway(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/v1/magpie/trace'):
            time.sleep(.08)
        with lock:
            if self.path.startswith('/v1/magpie/trace'):
                requests['trace'] += 1
                data = {key: state[key] for key in ('pid', 'seq', 'ids', 'routes')}
            elif self.path.startswith('/v1/magpie/concurrency'):
                requests['concurrency'] += 1
                data = {'active': state['active']}
            elif self.path.startswith('/v1/magpie/quotas'):
                data = {'data': []}
            elif self.path.startswith('/v1/magpie/usage'):
                data = {'calls': 0, 'cost': 0, 'unpriced': 0}
            else:
                self.send_error(404)
                return
            body = json.dumps(data).encode()
        try:
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (ConnectionResetError, BrokenPipeError):
            pass

    def log_message(self, *args):
        pass


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', type=Path, default=ROOT / 'src-tauri/target-decision/release/magpie-hud.exe')
    args = parser.parse_args()
    scratch = Path(tempfile.mkdtemp(prefix='hud-decision-'))
    for name in ('user', 'codex', 'local', 'webview'):
        (scratch / name).mkdir()
    usage = scratch / 'usage.jsonl'
    usage.write_text('', encoding='utf-8')
    state['routes'] = [route()]
    state['active'] = [{'id': 1, 'route': 41, 'session': 'decision-fixture', 'agent': 'claude-desktop',
                        'model': 'group/smart-split', 'phase': 'upstream', 'started': now, 'ms': 1000}]
    server = ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config = scratch / 'appdata/magpie-hud/config.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'usageJsonlPath': str(usage), 'magpieHost': f'127.0.0.1:{server.server_port}',
                                  'alwaysOnTop': False, 'window': {'x': 40, 'y': 40, 'width': 900, 'height': 720}}), encoding='utf-8')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = os.environ.copy()
    env.update(APPDATA=str(scratch / 'appdata'), LOCALAPPDATA=str(scratch / 'local'),
               USERPROFILE=str(scratch / 'user'), CODEX_HOME=str(scratch / 'codex'),
               WEBVIEW2_USER_DATA_FOLDER=str(scratch / 'webview'),
               WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f'--remote-debugging-port={port} --remote-allow-origins=http://127.0.0.1:{port}')
    process = subprocess.Popen([str(args.exe.resolve())], cwd=ROOT, env=env)
    client = None
    http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        deadline = time.monotonic() + 35
        debug_responses, debug_pages = 0, 0
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError(f'isolated native process exited: {process.returncode}')
            try:
                pages = json.load(http.open(f'http://127.0.0.1:{port}/json/list', timeout=1))
                debug_responses += 1
                debug_pages = len(pages)
                page = next(p for p in pages if p.get('type') == 'page' and 'tauri.localhost' in p.get('url', ''))
                client = CDP(page['webSocketDebuggerUrl'])
                break
            except (OSError, ValueError, StopIteration):
                time.sleep(.1)
        if not client:
            with lock:
                counts = dict(requests)
            raise AssertionError(f'native WebView unavailable: debugResponses={debug_responses}, pages={debug_pages}, gateway={counts}')
        is_isolated = client.evaluate(f"(async () => {{ const value = await window.__TAURI__.core.invoke('startup_state'); return value.snapshot.magpieHost === '127.0.0.1:{server.server_port}' && !value.snapshot.error; }})()")
        assert is_isolated, 'native HUD did not load isolated gateway configuration'
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            with lock:
                contacted = all(requests.values())
            if contacted:
                break
            time.sleep(.1)
        assert contacted, 'native HUD did not contact both isolated gateway endpoints'
        try:
            client.wait("document.querySelector('#decision-content')?.textContent.includes('规则5')", timeout=15)
        except AssertionError:
            debug = client.evaluate("({card:!!document.querySelector('.route-card'),api:!!window.__TAURI__,decision:!!window.hudDecision})")
            print(json.dumps({'diagnostic': debug, 'isolatedGateway': contacted}))
            raise
        assert client.evaluate("document.querySelector('#decision-content').textContent.includes('第7轮') && document.querySelector('#decision-content .decision-step')._lines.some(s=>s.includes('候选基础排序：按顺序'))")
        assert client.evaluate("[...document.querySelectorAll('.decision-step')].map(e=>e.textContent).join('|').indexOf('自动') < [...document.querySelectorAll('.decision-step')].map(e=>e.textContent).join('|').indexOf('干活')")
        assert client.evaluate("document.querySelector('#decision-content').textContent.includes('model-b')")
        assert client.evaluate("document.querySelector('#decision-content .decision-step')._lines.some(s=>s.includes('代码分析'))")
        assert client.evaluate("document.querySelector('#decision-content .decision-origin[data-origin=reused]') !== null")
        assert client.evaluate("document.querySelectorAll('.decision-wire:not(.decision-packet)').length === document.querySelectorAll('.decision-step').length - 1")
        assert client.evaluate("document.querySelector('#decision-route').getBoundingClientRect().height === 76")
        assert client.evaluate("Math.abs(document.querySelector('.sidebar').getBoundingClientRect().bottom-document.querySelector('#decision-route').getBoundingClientRect().bottom)<1")
        assert client.evaluate("(() => { const first=document.querySelector('.decision-step'); first.focus(); return document.querySelector('#decision-detail').textContent.includes('代码分析'); })()")
        client.evaluate("document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape'}))")
        assert client.evaluate("document.querySelector('#decision-detail').hidden")
        assert not client.evaluate("document.querySelector('#decision-route').textContent.includes('PRIVATE-EXPRESSION')")
        with lock:
            state['routes'][0]['done'] = True
            state['routes'][0]['status'] = 200
            state['routes'][0]['tries'][-1]['done'] = True
            state['routes'][0]['tries'][-1]['status'] = 200
            state['active'] = []
            state['seq'] += 1
        client.wait("document.querySelector('#decision-status').textContent === '已完成'", timeout=10)
        assert client.evaluate("document.querySelector('#decision-content .decision-step')._lines.some(s=>s.includes('test/classifier-v2'))")
        assert client.evaluate("[...document.querySelectorAll('.decision-step')].every(e=>!e.dataset.activity)")
        client.next_id += 1
        client.socket.send(json.dumps({'id': client.next_id, 'method': 'Page.captureScreenshot', 'params': {'format': 'png'}}))
        while True:
            response = json.loads(client.socket.recv())
            if response.get('id') == client.next_id:
                output = ROOT / '.codex_tmp/decision-route-native.png'
                output.parent.mkdir(exist_ok=True)
                output.write_bytes(base64.b64decode(response['result']['data']))
                break
        print(json.dumps({'passed': True, 'classifier': 'custom', 'fallbackAttempts': 2, 'retainedStatus': 'success'}))
    finally:
        if client:
            client.socket.close()
        process.terminate()
        try:
            process.wait(8)
        except subprocess.TimeoutExpired:
            process.kill()
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    main()
