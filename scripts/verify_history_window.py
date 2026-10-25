"""Isolated native history IPC/UI test: temporary logs and a loopback gateway only."""
import argparse
import base64
import datetime as dt
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('history_cdp', ROOT / 'scripts/verify_routing_window.py')
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)
now = dt.datetime.now().astimezone()
state = {'seq': 1, 'routes': [], 'active': []}
lock = threading.Lock()


def stamp(seconds=0):
    return (now + dt.timedelta(seconds=seconds)).isoformat(timespec='milliseconds')


def record(ident, seconds, session='history-main', **extra):
    value = {'route_id': ident, 't': stamp(seconds), 'session': session, 'agent': 'claude-desktop',
             'provider': 'codex', 'providerAccount': 'fixture@example.com', 'model': 'gpt-fixture',
             'effort': 'high', 'status': 200, 'in': 100, 'out': 120, 'cache_read': 80,
             'ms': 3000, 'ttft_ms': 1000}
    value.update(extra)
    return value


def trace(ident, seconds, child='', done=True):
    return {'id': ident, 'time': stamp(seconds), 'session': 'history-main', 'agent': 'claude-desktop',
            'provider': 'codex', 'subagentId': child, 'done': done, 'status': 200 if done else 0,
            'group': {'name': '自动', 'subs': [{'id': 'worker', 'name': '干活'}]},
            'order': [{'id': 'fixture', 'provider': 'codex', 'who': 'fixture@example.com', 'model': 'gpt-fixture', 'via': ['worker']}],
            # 按时间正序配对才符合真实流量：route 越大时间越早，因此 (6,5)、(4,3)、(2,1)
            # 各成一轮，较早的偶数号为开口、较晚的奇数号为续行。
            # 同时覆盖 held 续行和真实流量常见的 waits 续行（未持有轮初判定）。
            # 其余请求各自成轮，route 900 是运行中的独立一轮。
            'rule': {'use': 'group/worker', 'pick': 'high',
                     'turn': (66 - ident) // 2 if ident <= 6 else ident,
                     'held': ident == 1,
                     'waits': ident in (3, 5),
                     'when': ['PRIVATE-EXPRESSION'],
                     'classified': {'by': 'typesafe/jev-latest', 'intent': 'PRIVATE-INTENT-与排查类相关的超长意图描述文本',
                                    'intents': ['PRIVATE-INTENT-与排查类相关的超长意图描述文本'], 'sure': 0.55}},
            'tries': [{'id': 'fixture', 'model': 'gpt-fixture', 'effort': 'high', 'done': done,
                       'status': 200 if done else 0, 'start': stamp(seconds)}]}


class Gateway(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.startswith('/v1/magpie/trace'):
            time.sleep(.08)
        with lock:
            if self.path.startswith('/v1/magpie/trace'):
                data = {'seq': state['seq'], 'pid': 901, 'ids': [r['id'] for r in state['routes']], 'routes': state['routes']}
            elif self.path.startswith('/v1/magpie/concurrency'):
                data = {'active': state['active']}
            elif self.path.startswith('/v1/magpie/quotas'):
                data = {'data': []}
            elif self.path.startswith('/v1/magpie/usage'):
                data = {'calls': 1, 'cost': .1, 'unpriced': 0}
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


def command(client, method, params):
    client.next_id += 1
    client.socket.send(json.dumps({'id': client.next_id, 'method': method, 'params': params}))
    while True:
        message = json.loads(client.socket.recv())
        if message.get('id') == client.next_id:
            assert 'error' not in message, message
            return message.get('result', {})


def query(client, child=None, limit=50):
    args = json.dumps({'session': 'history-main', 'subagent': child, 'limit': limit})
    return client.evaluate(f"window.__TAURI__.core.invoke('query_route_history', {args})")


def main():
    scratch = Path(tempfile.mkdtemp(prefix='hud-history-'))
    usage = scratch / 'usage.jsonl'
    records = [record(i, -i * 45) for i in range(1, 66)]
    records[3]['status'] = 499
    records[6]['status'] = 503
    records[1].update(served='reported-fixture', tries=3)
    records[2].update({'in': 100000, 'out': 20000, 'cache_read': 500})
    records += [records[0], record(400, -4, subagent='child-one'), record(500, -5, handed_on=True, status=503),
                record(600, -6, session='other-session')]
    usage.write_text(''.join(json.dumps(r) + '\n' for r in records) + '{"incomplete":', encoding='utf-8')
    # 只为前几行给 trace：轮次序号相同的相邻请求（1/2、3/4）应折叠，其余行无轮次各自成组。
    state['routes'] = [trace(route, -route * 45) for route in range(1, 7)] + [
        trace(400, -4, child='child-one'), trace(900, 0, done=False)]
    state['active'] = [{'id': 90, 'route': 900, 'session': 'history-main', 'agent': 'claude-desktop',
                        'model': 'group/auto', 'provider': 'codex', 'phase': 'upstream', 'started': stamp(), 'ms': 1200}]
    server = ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    config = scratch / 'appdata/magpie-hud/config.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'usageJsonlPath': str(usage), 'magpieHost': f'127.0.0.1:{server.server_port}',
                                 'alwaysOnTop': False, 'window': {'x': 40, 'y': 40, 'width': 1120, 'height': 720}}), encoding='utf-8')
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
    env = os.environ.copy()
    env.update(APPDATA=str(scratch / 'appdata'), LOCALAPPDATA=str(scratch / 'local'),
               WEBVIEW2_USER_DATA_FOLDER=str(scratch / 'webview'),
               WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f'--remote-debugging-port={port}')
    parser = argparse.ArgumentParser()
    parser.add_argument('--exe', default='src-tauri/target-v008/release/magpie-hud.exe')
    options = parser.parse_args()
    exe = ROOT / options.exe
    process = subprocess.Popen([str(exe)], cwd=ROOT, env=env)
    client = None
    http = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            try:
                pages = json.load(http.open(f'http://127.0.0.1:{port}/json/list', timeout=1))
                page = next(p for p in pages if p.get('type') == 'page' and 'tauri.localhost' in p.get('url', ''))
                client = helpers.CDP(page['webSocketDebuggerUrl'])
                break
            except (OSError, ValueError, StopIteration):
                time.sleep(.1)
        assert client, 'Native WebView unavailable'
        client.wait("!!document.querySelector('.route-card[data-history-scope]')", timeout=12)
        # 等待 trace 与活跃数据均可查询；held 和 waits 续行都应挂回开口组。
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            data = query(client, limit=200)
            folded_groups = [g for g in data['groups'] if g['count'] > 1]
            if len(folded_groups) >= 3 and any(r['outcome'] in ('running', 'waiting') for r in data['rows']):
                break
            time.sleep(.1)
        rows = data['rows']
        groups = data['groups']
        assert len(rows) == 66, data
        assert all(not r.get('subagent') for r in rows), rows
        assert sum(r.get('routeId') == 1 for r in rows) == 1
        assert not any(r.get('routeId') in (400, 500, 600) for r in rows)
        # 连续且轮次相同的请求折叠成一组；route 1 有两条不同时间的记录，
        # 其中较早的一条与 route 2 同轮，组在那里合并。
        folded = [g for g in groups if g['count'] > 1]
        assert folded, groups
        assert all(len({r['turn'] for r in g['requests']}) == 1 for g in folded), folded
        live_group = next(g for g in groups if any(r.get('routeId') == 900 for r in g['requests']))
        live = next(r for r in live_group['requests'] if r.get('routeId') == 900)
        assert live['tokens'] is None and live['tokensPerSecond'] is None, live
        completed_group = next(g for g in groups if any(r.get('routeId') == 1 for r in g['requests']))
        completed = next(r for r in completed_group['requests'] if r.get('routeId') == 1)
        assert completed['tokens'] == 300 and completed['tokensPerSecond'] == 60, completed
        assert completed['routeGroups'] == ['自动', '干活'], completed
        assert completed['ruleTarget'] == 'group/worker' and completed['ruleEffort'] == 'high', completed
        assert completed['turn'] is not None, completed
        assert 'PRIVATE-EXPRESSION' not in json.dumps(data)
        child = query(client, child='child-one')
        assert len(child['rows']) == 1 and child['rows'][0]['routeId'] == 400, child
        limited = query(client)
        assert len(limited['groups']) == 50 and limited['rowLimited'] and limited['groupLimited'], limited
        # Drive actual hover, transfer into the panel, and delayed leave dismissal.
        box = client.evaluate("(()=>{const c=[...document.querySelectorAll('.route-card')].find(e=>e.dataset.historyScope==='[\"history-main\",\"\"]');const b=c.getBoundingClientRect();return {x:b.left+b.width/2,y:b.top+b.height/2}})()")
        command(client, 'Input.dispatchMouseEvent', {'type': 'mouseMoved', **box})
        client.wait("!document.querySelector('#route-history').hidden", timeout=3)
        panel_box = client.evaluate("(()=>{const b=document.querySelector('#route-history').getBoundingClientRect();return{x:b.left+20,y:b.top+20}})()")
        command(client, 'Input.dispatchMouseEvent', {'type': 'mouseMoved', **panel_box})
        time.sleep(.3)
        assert not client.evaluate("document.querySelector('#route-history').hidden")
        command(client, 'Input.dispatchMouseEvent', {'type': 'mouseMoved', 'x': 2, 'y': 2})
        client.wait("document.querySelector('#route-history').hidden", timeout=3)
        client.evaluate("(()=>{const c=[...document.querySelectorAll('.route-card')].find(e=>e.dataset.historyScope==='[\"history-main\",\"\"]');c.focus();c.dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowDown',altKey:true,bubbles:true}));})()")
        client.wait("!document.querySelector('#route-history').hidden && document.querySelectorAll('.history-table tbody tr').length===50")
        columns = client.evaluate("[...document.querySelectorAll('.history-table th')].map(e=>e.textContent)")
        # 首列是展开按钮列，表头无文字，用 aria-label 说明。
        assert columns == ['', '开始时间', '意图', '判定', '模型', '强度', 'Token', 'TPS', '状态'], columns
        assert client.evaluate("document.querySelector('.history-table th').getAttribute('aria-label')") == '展开'
        assert client.evaluate("[...document.querySelectorAll('.history-table th,.history-table td')].every(e=>getComputedStyle(e).textAlign==='center')")
        assert client.evaluate("(()=>{const v=document.querySelector('.history-viewport');return v.scrollWidth===v.clientWidth})()")
        assert client.evaluate("[...document.querySelectorAll('.history-decision')].every(e=>!e.textContent.includes('判定 high'))")
        assert client.evaluate("!!document.querySelector('.history-effort .effort[data-level=high]')")
        assert client.evaluate("document.querySelectorAll('.history-prompt').length") == 0
        assert client.evaluate("[...document.querySelectorAll('.history-model')].every(e=>!e.textContent.includes(' → '))")
        assert client.evaluate("!!document.querySelector('.history-intent')") and client.evaluate("[...document.querySelectorAll('.history-intent')].every(e=>!e.textContent.includes('PRIVATE-INTENT'))")
        assert client.evaluate("[...document.querySelectorAll('.history-intent')].some(e=>e.textContent.endsWith('…'))")
        # 组行的 Token 是组内合计，精确值口径在组行说明里。
        # 展开唯一含大用量请求的多请求组：子行出现后，120.5k 所在子行的说明含精确值。
        client.evaluate("document.querySelector('tr[data-foldable] .history-expand-button').click()")
        client.wait("document.querySelectorAll('tr[data-child-of]').length>0")
        assert client.evaluate("document.querySelector('tr[data-foldable][data-expanded] .history-count').textContent.trim().startsWith('×')")
        client.evaluate("(()=>{for(const b of document.querySelectorAll('tr[data-foldable]:not([data-expanded]) .history-expand-button'))b.click();})()")
        client.wait("[...document.querySelectorAll('tr[data-child-of] td.history-number')].some(e=>e.textContent==='120.5k')")
        token_detail = client.evaluate("(()=>{const td=[...document.querySelectorAll('tr[data-child-of] td.history-number')].find(e=>e.textContent==='120.5k');return td ? td.querySelector('.history-cell').title : null})()")
        assert token_detail and '120,500' in token_detail, token_detail
        client.evaluate("(()=>{for(const b of document.querySelectorAll('tr[data-expanded] .history-expand-button'))b.click();})()")
        client.wait("document.querySelectorAll('tr[data-child-of]').length===0")
        client.evaluate("document.querySelector('.history-footer button').click()")
        client.wait("document.querySelectorAll('.history-table tbody tr').length>50")
        client.evaluate("document.querySelector('#route-history button[aria-label=\"固定当前履历\"]').click();document.querySelector('.history-viewport').scrollTop=250")
        time.sleep(.6)
        assert client.evaluate("document.querySelector('.history-viewport').scrollTop") == 250
        # 自动刷新验收：不用刷新按钮，完成记录应在约一秒内进入已打开的履历。
        completed_at = time.perf_counter()
        # Complete the final partial line, then append the final request row.
        with usage.open('a', encoding='utf-8') as stream:
            stream.write('0}\n')
            stream.write(json.dumps(record(900, 0, served='reported-live')) + '\n')
        with lock:
            state['routes'][-1] = trace(900, 0, done=True)
            state['active'] = []
            state['seq'] += 1
        deadline = time.monotonic() + 8
        appeared_in_ms = None
        while time.monotonic() < deadline:
            final = query(client, limit=200)
            match = [r for r in final['rows'] if r.get('routeId') == 900]
            if len(match) == 1 and match[0]['outcome'] == 'success':
                appeared_in_ms = round((time.perf_counter() - completed_at) * 1000)
                break
            time.sleep(.05)
        assert appeared_in_ms is not None and appeared_in_ms <= 1800, f'新请求应在秒级（<=1.8s）完成，实测 {appeared_in_ms}ms'
        assert len(match) == 1 and match[0]['tokens'] == 300 and match[0]['served'] == 'reported-live', match
        client.wait("[...document.querySelectorAll('.history-table tbody tr')].some(r=>r.dataset.outcome==='success'&&r.querySelector('.history-model').textContent==='reported-live')")
        assert client.evaluate("document.querySelector('.history-viewport').scrollTop") == 250
        client.evaluate("document.querySelector('.history-viewport').scrollTop=0")
        # 交付截图直接展示 waits 续行组及展开后的子请求。
        waits_group = next(g for g in final['groups'] if any(r.get('routeId') == 3 for r in g['requests']))
        assert waits_group['count'] == 2, 'waits 续行应与开口合并'
        group_selector = json.dumps(f'tr[data-group-id="{waits_group["id"]}"] .history-expand-button')
        client.evaluate(f"document.querySelector({group_selector}).click()")
        client.wait("document.querySelectorAll('tr[data-child-of]').length===2")
        shot = command(client, 'Page.captureScreenshot', {'format': 'png'})
        (ROOT / 'docs/showcase/hud-request-history.png').write_bytes(base64.b64decode(shot['data']))
        command(client, 'Input.dispatchKeyEvent', {'type': 'keyDown', 'key': 'Escape', 'code': 'Escape'})
        assert client.evaluate("document.querySelector('#route-history').hidden")
        report = {'passed': True, 'rootRows': len(final['rows']), 'childRows': len(child['rows']),
                  'appearedInMs': appeared_in_ms,
                  'liveMetricsNull': True, 'liveCompletedOnce': True, 'ruleWhitelist': True,
                  'rootChildIsolation': True, 'limited50': True, 'expanded200': True, 'pinScrollRetained': True,
                  'fontPx': client.evaluate("getComputedStyle(document.querySelector('.history-table')).fontSize")}
        (ROOT / '.codex_tmp/history-native-result.json').write_text(json.dumps(report, indent=2), encoding='utf-8')
        print(json.dumps(report))
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
