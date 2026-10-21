"""隔离启动实际 Tauri WebView；依赖 websocket-client，只写临时日志，不改生产配置。"""
import datetime
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import urllib.request

import websocket

ROOT = Path(__file__).resolve().parents[1]


class CDP:
    """通过本次测试显式开启的 WebView2 调试端口检查实际窗口，不触碰 Magpie。"""
    def __init__(self, url):
        self.socket = websocket.create_connection(url, timeout=5, suppress_origin=True)
        self.next_id = 0

    def evaluate(self, expression):
        """在实际产品页面执行只用于验证的 DOM 查询或 UI 事件。"""
        self.next_id += 1
        self.socket.send(json.dumps({"id": self.next_id, "method": "Runtime.evaluate", "params": {"expression": expression, "returnByValue": True, "awaitPromise": True}}))
        while True:
            message = json.loads(self.socket.recv())
            if message.get("id") == self.next_id:
                if "error" in message or "exceptionDetails" in message.get("result", {}):
                    raise AssertionError(message)
                return message.get("result", {}).get("result", {}).get("value")

    def wait(self, expression, timeout=6):
        """等待实际 DOM 条件收敛，返回从调用到满足条件的毫秒数。"""
        start = time.perf_counter()
        while time.perf_counter() - start < timeout:
            if self.evaluate(expression):
                return (time.perf_counter() - start) * 1000
            time.sleep(.015)
        raise AssertionError(f"DOM condition timed out: {expression}")


def record(number, account, status=200, session="session-alpha", **extra):
    """创建脱敏样本；t 代表请求开始时刻，route_id 提供稳定唯一键。"""
    data = {"route_id": number, "t": (datetime.datetime.now().astimezone() + datetime.timedelta(seconds=number)).isoformat(), "session": session, "agent": "claude-desktop", "providerAccount": account, "provider": "codex", "model": "gpt-6.1-sol", "req": "group/smart-split", "effort": "xhigh", "status": status, "in": 100, "out": 100, "cache_read": 800, "ms": 3000, "ttft_ms": 1000}
    data.update(extra)
    return data


def append(path, data):
    """只写临时测试日志，并刷新到磁盘，供文件监视验收。"""
    with path.open("a", encoding="utf-8", newline="\n") as stream:
        stream.write(json.dumps(data) + "\n")
        stream.flush()
        os.fsync(stream.fileno())


def main():
    """启动真实应用，驱动会话选择和失败恢复，最后关闭隔离实例。"""
    scratch = Path(tempfile.mkdtemp(prefix="magpie-hud-routing-"))
    appdata = scratch / "appdata"
    config_dir = appdata / "magpie-hud"
    config_dir.mkdir(parents=True)
    path = scratch / "usage.jsonl"
    append(path, record(1, "failed@example.com", 503, err="temporary upstream failure"))
    (config_dir / "config.json").write_text(json.dumps({"usageJsonlPath": str(path), "magpieHost": "127.0.0.1:1", "window": None, "alwaysOnTop": False}), encoding="utf-8")
    with socket.socket() as available:
        available.bind(("127.0.0.1", 0))
        port = available.getsockname()[1]
    env = os.environ.copy()
    env["APPDATA"] = str(appdata)
    env["WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"] = f"--remote-debugging-port={port} --remote-allow-origins=http://127.0.0.1:{port}"
    env["WEBVIEW2_USER_DATA_FOLDER"] = str(scratch / "webview")
    process = subprocess.Popen([str(ROOT / "src-tauri/target/debug/magpie-hud.exe")], cwd=ROOT, env=env)
    client = None
    try:
        deadline = time.monotonic() + 40
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=1) as response:
                    pages = json.load(response)
                page = next((page for page in pages if page.get("type") == "page" and "tauri.localhost" in page.get("url", "")), None)
                if page:
                    client = CDP(page["webSocketDebuggerUrl"])
                    break
            except (OSError, ValueError):
                pass
            time.sleep(.1)
        assert client is not None, "WebView2 debug page not ready"
        client.wait("document.querySelectorAll('.route-card.failed').length === 1")
        assert client.evaluate("document.getElementById('line1-kind').textContent") == "失败"
        append_started = time.perf_counter()
        append(path, record(2, "success@example.com", credits=2.5))
        client.wait("document.querySelectorAll('.route-card').length === 2 && document.getElementById('daily-tokens').textContent === '2.0k'")
        elapsed = (time.perf_counter() - append_started) * 1000
        assert elapsed < 500, f"file-to-render {elapsed:.1f}ms exceeds 500ms"
        assert client.evaluate("document.getElementById('daily-cost').textContent") == "2.50"
        append(path, record(3, "failed@example.com"))
        client.wait("document.querySelectorAll('.route-card.recovered').length === 1")
        assert client.evaluate("document.querySelectorAll('.route-card.failed').length") == 0
        append(path, record(4, "other@example.com", session="session-beta", model="grok-4.7-build-fast@500k", effort="high", tries=2))
        client.wait("document.getElementById('session-select').options.length === 3")
        client.evaluate("(() => { const s=document.getElementById('session-select'); s.value='session-alpha'; s.dispatchEvent(new Event('change')); })()")
        assert client.evaluate("document.getElementById('daily-tokens').textContent") == "3.0k"
        assert client.evaluate("document.getElementById('stats-scope').textContent") == "未命名会话"
        assert client.evaluate("document.querySelectorAll('.note-row').length") == 2
        assert client.evaluate("document.querySelector('.card-copy').children.length") == 2
        # 半行不渲染；补换行后才计入，重复触发不重复统计。
        partial = json.dumps(record(5, "other@example.com", session="session-beta"))
        with path.open("a", encoding="utf-8") as stream:
            stream.write(partial)
            stream.flush()
        client.evaluate("(() => { const s=document.getElementById('session-select'); s.value='session-beta'; s.dispatchEvent(new Event('change')); })()")
        assert client.evaluate("document.getElementById('request-count').textContent") == '1 次请求'
        time.sleep(.2)
        assert client.evaluate("document.getElementById('request-count').textContent") == '1 次请求'
        with path.open("a", encoding="utf-8") as stream:
            stream.write("\n")
        client.wait("document.getElementById('request-count').textContent === '2 次请求'")
        client.evaluate("(() => { const s=document.getElementById('session-select'); s.value='*'; s.dispatchEvent(new Event('change')); })()")
        client.wait("document.getElementById('request-count').textContent === '5 次请求'")
        values = client.evaluate("({cards:document.querySelectorAll('.route-card').length, rows:document.querySelectorAll('.note-row').length, stats:[...document.querySelectorAll('.metric dd')].map(e=>e.textContent), status:[...document.querySelectorAll('.state-tag')].map(e=>e.textContent), iconLoaded:[...document.images].every(i=>i.complete&&i.naturalWidth>0)})")
        assert values["iconLoaded"]
        report = {"passed": True, "fileToRenderMs": round(elapsed, 1), "view": values, "scope": "isolated fixture in actual Tauri WebView2"}
        (scratch / "result.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
        print(json.dumps(report, ensure_ascii=False))
        print(scratch)
    finally:
        if client:
            try:
                client.evaluate("setTimeout(() => window.__TAURI__.webviewWindow.getCurrentWebviewWindow().close(), 100)")
            except (websocket.WebSocketException, OSError):
                pass  # 窗口已关闭时调试连接可以先于命令应答消失。
            finally:
                client.socket.close()
        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=5)


if __name__ == "__main__":
    main()
