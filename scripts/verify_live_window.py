"""在实际 WebView2 验收实时状态、标题映射及横竖缩放；依赖 websocket-client。

测试只写临时目录，模拟网关绑定回环地址，退出时仅关闭测试自己启动的窗口。
"""
import base64
import datetime
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

from verify_routing_window import CDP, append, record

ROOT = Path(__file__).resolve().parents[1]


class Gateway(BaseHTTPRequestHandler):
    """最小真实 HTTP 契约，锁内更新活跃与路由，模拟执行到完成。"""
    state = {}
    lock = threading.Lock()

    def do_GET(self):
        """只回答只读遥测；trace 以短暂等待模拟长轮询，其他路径拒绝。"""
        if self.path.startswith('/v1/magpie/trace'):
            time.sleep(.12)
        with self.lock:
            if self.path.startswith('/v1/magpie/trace'):
                data = {key: self.state[key] for key in ('seq', 'pid', 'ids', 'routes')}
            elif self.path == '/v1/magpie/concurrency':
                data = {'active': self.state['active']}
            elif self.path == '/v1/magpie/quotas?kept=1':
                data = {'object': 'list', 'data': [{'provider': 'grok', 'name': 'grok', 'user': 'test@example.com', 'kind': 'subscription', 'readAt': self.state['started'], 'windows': [{'name': '5 hours', 'remaining': 62, 'used': 38, 'resetsAt': self.state['started']}, {'name': '7 days', 'remaining': 21, 'used': 79}]}]}
            elif self.path == '/v1/magpie/usage?period=today':
                data = {'since': self.state['started'], 'calls': 2, 'cost': 0.42, 'unpriced': 1, 'sessions': [{'id': 'session-alpha', 'calls': 1, 'cost': .12, 'unpriced': 0}]}
            else:
                self.send_error(404)
                return
            body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        """保持测试输出只有结论，不打印周期性轮询。"""


def free_port():
    """分配测试 WebView 调试端口，释放后交给其子进程监听。"""
    with socket.socket() as available:
        available.bind(('127.0.0.1', 0))
        return available.getsockname()[1]


def screenshot(client, name):
    """直接保存实际 WebView 的截图，不生成或替换页面内容。"""
    client.next_id += 1
    client.socket.send(json.dumps({'id': client.next_id, 'method': 'Page.captureScreenshot', 'params': {'format': 'png'}}))
    while True:
        result = json.loads(client.socket.recv())
        if result.get('id') == client.next_id:
            path = ROOT / 'docs/showcase' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(base64.b64decode(result['result']['data']))
            return str(path)


def command(client, method, params):
    """发送浏览器调试检查命令；不替换产品 DOM 或业务逻辑。"""
    client.next_id += 1
    client.socket.send(json.dumps({'id': client.next_id, 'method': method, 'params': params}))
    while True:
        result = json.loads(client.socket.recv())
        if result.get('id') == client.next_id:
            assert 'error' not in result, result
            return result.get('result', {})


def acrylic_enabled(pid):
    """Windows 11 上读回 DWM backdrop type，确认原生磨砂已应用。"""
    import ctypes
    from ctypes import wintypes
    windows = []
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    @callback_type
    def visit(hwnd, _data):
        owner = wintypes.DWORD()
        ctypes.windll.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and ctypes.windll.user32.IsWindowVisible(hwnd):
            windows.append(hwnd)
        return True

    ctypes.windll.user32.EnumWindows(visit, 0)
    for hwnd in windows:
        backdrop = ctypes.c_int()
        result = ctypes.windll.dwmapi.DwmGetWindowAttribute(wintypes.HWND(hwnd), 38, ctypes.byref(backdrop), ctypes.sizeof(backdrop))
        if result == 0 and backdrop.value == 3:
            return True
    return False


def main():
    """启动隔离实例，通过真实 HTTP、IPC、原生尺寸和最终日志完成验收。"""
    scratch = Path(tempfile.mkdtemp(prefix='magpie-hud-live-'))
    path = scratch / 'usage.jsonl'
    path.write_text('', encoding='utf-8')
    appdata, localdata = scratch / 'appdata', scratch / 'localdata'
    title_dir = localdata / 'Claude-3p/claude-code-sessions/profile/org'
    title_dir.mkdir(parents=True)
    (title_dir / 'local_demo.json').write_text(json.dumps({'sessionId': 'local_demo', 'cliSessionId': 'session-alpha', 'title': 'HUD / 实时会话'}), encoding='utf-8')
    started = datetime.datetime.now().astimezone().isoformat()
    active = {'id': 1, 'route': 100, 'session': 'session-alpha', 'agent': 'claude-desktop', 'model': 'group/smart-split', 'provider': 'grok', 'phase': 'upstream', 'started': started, 'ms': 1200}
    route = {'id': 100, 'session': 'session-alpha', 'provider': 'grok', 'done': False,
             'order': [{'id': 'grok', 'who': 'test@example.com'}],
             'tries': [{'id': 'grok', 'model': 'grok-4.7-build-fast@500k', 'effort': 'high', 'done': False}]}
    Gateway.state = {'seq': 1, 'pid': 1000, 'ids': [100], 'routes': [route], 'active': [active], 'started': started}
    server = ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    cfg = appdata / 'magpie-hud/config.json'
    cfg.parent.mkdir(parents=True)
    cfg.write_text(json.dumps({'usageJsonlPath': str(path), 'magpieHost': f'127.0.0.1:{server.server_port}', 'alwaysOnTop': False, 'window': None}), encoding='utf-8')
    port = free_port()
    env = os.environ.copy()
    env.update(APPDATA=str(appdata), LOCALAPPDATA=str(localdata), CODEX_HOME=str(scratch / 'codex'),
               WEBVIEW2_USER_DATA_FOLDER=str(scratch / 'webview'),
               WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f'--remote-debugging-port={port}')
    process = subprocess.Popen([str(ROOT / 'src-tauri/target/debug/magpie-hud.exe')], cwd=ROOT, env=env)
    client = None
    try:
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list', timeout=1) as response:
                    pages = json.load(response)
                page = next((p for p in pages if p.get('type') == 'page' and 'tauri.localhost' in p.get('url', '')), None)
                if page:
                    client = CDP(page['webSocketDebuggerUrl'])
                    break
            except (OSError, ValueError):
                pass
            time.sleep(.1)
        assert client, 'WebView2 not ready'
        assert acrylic_enabled(process.pid), 'Windows 11 DWM acrylic backdrop not enabled'
        client.wait("document.querySelector('.route-card.running .session-title')?.textContent === 'HUD / 实时会话'")
        client.wait("document.querySelector('.quota-track')?.getAttribute('aria-valuenow') === '38' && document.getElementById('daily-cost').textContent === '$0.42'")
        assert client.evaluate("[...document.querySelectorAll('.quota-item')].every(item => item.children.length === 2)")
        assert client.evaluate("document.querySelector('.quota-window strong').textContent") == '38%'
        assert client.evaluate("Math.abs(document.querySelector('.quota-fill').getBoundingClientRect().width / document.querySelector('.quota-track').getBoundingClientRect().width - .38) < .02")
        assert client.evaluate("document.querySelector('.route-card.running .state-tag').textContent") == ''
        assert client.evaluate("document.querySelector('.request-spinner circle').getAttribute('r')") == '7'
        client.wait("getComputedStyle(document.querySelector('.request-spinner')).animationName === 'request-spin'")
        rotation = client.evaluate("getComputedStyle(document.querySelector('.request-spinner')).transform")
        time.sleep(.15)
        assert client.evaluate("getComputedStyle(document.querySelector('.request-spinner')).transform") != rotation
        assert client.evaluate("new Set(['daily-tokens','daily-cost','daily-speed'].map(id=>getComputedStyle(document.getElementById(id)).color)).size") == 3
        assert client.evaluate("document.querySelector('h1').textContent") == 'Magpie HUD'
        assert client.evaluate("getComputedStyle(document.getElementById('hud')).backgroundColor") == 'rgba(23, 25, 30, 0.5)'
        assert client.evaluate("document.querySelector('.effort').dataset.level") == 'high'
        assert client.evaluate("document.getElementById('hud').dataset.sources") == 'compact'
        assert client.evaluate("document.querySelector('.source-node').getBoundingClientRect().bottom <= document.querySelector('.router-node').getBoundingClientRect().top")
        client.evaluate("(() => { const s=document.getElementById('session-select'); s.value='session-alpha'; s.dispatchEvent(new Event('change')); })()")
        assert client.evaluate("document.getElementById('daily-cost').textContent") == '$0.12'
        client.evaluate("(() => { const s=document.getElementById('session-select'); s.value='*'; s.dispatchEvent(new Event('change')); })()")
        assert client.evaluate("document.querySelector('.card-tokens').textContent") == '待统计 Token'
        client.wait("document.querySelector('.branch.running') && getComputedStyle(document.querySelector('.branch.running')).animationName === 'route-stream'")
        offset = client.evaluate("getComputedStyle(document.querySelector('.branch.running')).strokeDashoffset")
        time.sleep(.15)
        assert client.evaluate("getComputedStyle(document.querySelector('.branch.running')).strokeDashoffset") != offset
        command(client, 'Emulation.setEmulatedMedia', {'features': [{'name': 'prefers-reduced-motion', 'value': 'reduce'}]})
        client.wait("document.querySelector('.branch.running') && getComputedStyle(document.querySelector('.branch.running')).animationName === 'none'")
        assert client.evaluate("getComputedStyle(document.querySelector('.request-spinner')).animationName") == 'none'
        command(client, 'Emulation.setEmulatedMedia', {'features': []})
        assert client.evaluate("document.getElementById('request-count').textContent") == '0 次请求'
        wide = screenshot(client, 'hud-live-wide.png')
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(420,760))")
        client.wait("innerWidth===420 && document.getElementById('hud').dataset.layout==='tall'")
        assert client.evaluate("document.documentElement.scrollWidth <= innerWidth")
        assert client.evaluate("getComputedStyle(document.querySelector('.source-icon')).display !== 'none' && document.querySelector('.source-icon img').naturalWidth > 0 && document.querySelector('.router-emblem').getBoundingClientRect().width >= 54")
        tall = screenshot(client, 'hud-live-tall.png')
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(360,640))")
        client.wait("innerWidth===360 && document.getElementById('hud').dataset.layout==='tall'")
        assert client.evaluate("document.documentElement.scrollWidth <= innerWidth && document.querySelector('.card-metrics').getBoundingClientRect().right <= innerWidth")
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(360,280))")
        client.wait("innerWidth===360 && innerHeight===280")
        assert client.evaluate("document.documentElement.scrollWidth <= innerWidth && getComputedStyle(document.querySelector('.content')).overflowY === 'auto' && document.querySelector('.route-card').getBoundingClientRect().height > 0")
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(420,760))")
        client.wait("innerWidth===420")
        # 执行过程中重命名，不依赖新增 usage 记录才能刷新标题。
        (title_dir / 'local_demo.json').write_text(json.dumps({'sessionId': 'local_demo', 'cliSessionId': 'session-alpha', 'title': 'HUD / 名称更新'}), encoding='utf-8')
        client.wait("document.querySelector('.session-title').textContent === 'HUD / 名称更新'")
        with Gateway.lock:
            route['done'] = True
            route['tries'][0]['done'] = True
            Gateway.state['seq'] = 2
            Gateway.state['active'] = []
        append(path, record(1, 'test@example.com', model='grok-4.7-build-fast@500k', provider='grok', route_id=100))
        client.wait("!document.querySelector('.route-card.running') && document.querySelector('.card-tokens')?.textContent==='1.0k Token'")
        assert client.evaluate("document.querySelector('.card-tps').textContent") == '50.0 TPS'
        assert client.evaluate("document.querySelectorAll('.request-spinner').length") == 0
        assert client.evaluate("document.querySelectorAll('.note-row').length") == 2
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(1120,400))")
        client.wait("innerWidth===1120 && document.getElementById('hud').dataset.layout==='wide'")
        assert client.evaluate("document.documentElement.scrollWidth <= innerWidth")
        # Magpie 不可达时保留完成统计，并给出明确断线提示。
        server.shutdown()
        server.server_close()
        client.wait("document.getElementById('watch-label').textContent.includes('实时连接中断')", timeout=8)
        assert client.evaluate("document.querySelector('.card-tps').textContent") == '50.0 TPS'
        assert client.evaluate("document.querySelectorAll('.request-spinner').length") == 0
        client.evaluate("window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize(420,760))")
        client.wait("innerWidth===420 && innerHeight===760")
        client.evaluate('setTimeout(()=>window.__TAURI__.webviewWindow.getCurrentWebviewWindow().close(),50)')
        client.socket.close()
        client = None
        process.wait(timeout=8)
        saved = json.loads(cfg.read_text(encoding='utf-8'))
        assert saved['window']['width'] == 420 and saved['window']['height'] == 760
        env['WEBVIEW2_USER_DATA_FOLDER'] = str(scratch / 'webview-restart')
        process = subprocess.Popen([str(ROOT / 'src-tauri/target/debug/magpie-hud.exe')], cwd=ROOT, env=env)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f'http://127.0.0.1:{port}/json/list', timeout=1) as response:
                    pages = json.load(response)
                page = next((p for p in pages if p.get('type') == 'page' and 'tauri.localhost' in p.get('url', '')), None)
                if page:
                    client = CDP(page['webSocketDebuggerUrl'])
                    break
            except (OSError, ValueError):
                pass
            time.sleep(.1)
        assert client, 'restarted WebView2 not ready'
        client.wait("innerWidth===420 && innerHeight===760 && document.getElementById('hud')?.dataset.layout==='tall'")
        print(json.dumps({'passed': True, 'checks': ['two-line-usage-bars', 'dashed-spinner-rotation', 'statistic-colors', 'native-acrylic', 'quota-windows', 'global-and-session-cost', 'title-and-half-opacity', 'source-stacking-and-icons', 'effort-color', 'active-before-completion', 'real-title-and-rename', 'animation-moves', 'reduced-motion', 'token-and-tps', 'native-wide-tall-resize', 'minimum-width-360', 'disconnect-stops-activity', 'restart-restores-size'], 'screenshots': [wide, tall]}, ensure_ascii=False))
    finally:
        if client:
            try:
                client.evaluate('setTimeout(()=>window.__TAURI__.webviewWindow.getCurrentWebviewWindow().close(),50)')
            except Exception:
                pass
            client.socket.close()
        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)


if __name__ == '__main__':
    main()
