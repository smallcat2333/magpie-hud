"""在实际 WebView2 验收额度关联、会话切换与路由连线；依赖 websocket-client。

只写临时目录，模拟网关只绑定回环地址，退出时仅关闭测试自己启动的窗口。
不注入绘制或修复逻辑，几何只读取产品 DOM。
"""
import argparse
import base64
import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request

import websocket

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from verify_routing_window import CDP, append, record

ROOT = Path(__file__).resolve().parents[1]
GROK_ACCOUNT = "grok.account@example.com"
GPT_ACCOUNT = "gpt.selected@example.com"
EXTRA_ACCOUNT = "codex.unselected@example.com"
GROK_MODEL = "grok-4.7-build-fast@500k"
GPT_MODEL = "gpt-6.1-sol"
OTHER_MODEL = "gpt-6.1-other"
CANDIDATE_ID = "shared-candidate"


class Gateway(BaseHTTPRequestHandler):
    """最小真实 HTTP 契约；锁内切换场景，不访问生产 Magpie。"""

    state = {}
    lock = threading.Lock()

    def do_GET(self):
        """只回答只读遥测；trace 短暂等待模拟长轮询，其他路径拒绝。"""
        if self.path.startswith("/v1/magpie/trace"):
            time.sleep(.12)
        with self.lock:
            if self.path.startswith("/v1/magpie/trace"):
                data = {key: self.state[key] for key in ("seq", "pid", "ids", "routes")}
            elif self.path == "/v1/magpie/concurrency":
                data = {"active": self.state["active"]}
            elif self.path == "/v1/magpie/quotas?kept=1":
                self.state["quota_reads"] += 1
                self.state["quota_times"].append(time.monotonic())
                data = {"object": "list", "data": self.state["quotas"]}
            elif self.path == "/v1/magpie/usage?period=today":
                data = {"since": self.state["started"], "calls": 0, "cost": 0, "unpriced": 0, "sessions": []}
            else:
                self.send_error(404)
                return
            body = json.dumps(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        """保持测试输出只有阶段和结论，不打印周期性轮询。"""


def stage(name):
    """打印阶段，避免长时间等待时没有进度。"""
    print(json.dumps({"stage": name, "at": round(time.monotonic(), 3)}, ensure_ascii=False), flush=True)


def free_port():
    """分配测试 WebView 调试端口，释放后交给其子进程监听。"""
    with socket.socket() as available:
        available.bind(("127.0.0.1", 0))
        return available.getsockname()[1]


def connect(port, process, timeout=40):
    """连接本次测试窗口；记录调试端点状态，不猜测页面地址。"""
    deadline = time.monotonic() + timeout
    last_error = "endpoint not checked"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(f"window exited {process.returncode} before CDP")
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=1) as response:
                pages = json.load(response)
            stage_pages = [{"type": item.get("type"), "url": item.get("url"), "title": item.get("title")} for item in pages]
            last_error = json.dumps(stage_pages, ensure_ascii=False)
            page = next((item for item in pages if item.get("type") == "page" and (
                "tauri.localhost" in item.get("url", "") or "tauri://localhost" in item.get("url", "")
                or (item.get("url") == "about:blank" and "Magpie HUD" in item.get("title", ""))
            )), None)
            if page:
                stage(f"cdp-page {page.get('url')}")
                return CDP(page["webSocketDebuggerUrl"])
        except (OSError, ValueError, websocket.WebSocketException) as error:
            last_error = f"{type(error).__name__}: {error}"
        time.sleep(.25)
    raise AssertionError(f"WebView2 not ready: {last_error}")


def command(client, method, params):
    """发送浏览器调试检查命令；不替换产品 DOM 或业务逻辑。"""
    client.next_id += 1
    client.socket.send(json.dumps({"id": client.next_id, "method": method, "params": params}))
    while True:
        result = json.loads(client.socket.recv())
        if result.get("id") == client.next_id:
            assert "error" not in result, result
            return result.get("result", {})


def screenshot(client, scratch, name):
    """把实际 WebView 截图只写入本次临时目录，不改项目文件。"""
    client.next_id += 1
    client.socket.send(json.dumps({"id": client.next_id, "method": "Page.captureScreenshot", "params": {"format": "png"}}))
    while True:
        result = json.loads(client.socket.recv())
        if result.get("id") == client.next_id:
            path = scratch / name
            path.write_bytes(base64.b64decode(result["result"]["data"]))
            return str(path)


def quota(provider, user, used, resets_at, name=None):
    """构造一个精确账号额度；used 是已用百分比。"""
    return {
        "provider": provider, "name": name or provider, "user": user, "kind": "subscription",
        "readAt": datetime.datetime.now().astimezone().isoformat(),
        "windows": [{"name": "5 hours", "used": used, "remaining": 100 - used, "resetsAt": resets_at}],
    }


def live_route(route_id, session, order, tries, done=False):
    """构造外层路由；账号归属只由候选 who 决定。"""
    return {"id": route_id, "session": session, "provider": "grok", "done": done, "order": order, "tries": tries}


def active_request(request_id, route_id, session, model="group/smart-split", phase="upstream"):
    """构造进行中请求；外层 provider 故意保持 grok，真实归属看候选。"""
    started = datetime.datetime.now().astimezone().isoformat()
    return {
        "id": request_id, "route": route_id, "session": session, "agent": "claude-desktop",
        "model": model, "provider": "grok", "phase": phase, "started": started, "ms": 1200,
    }


def reset_at(seconds):
    """生成带 +5 秒缓冲的重置时刻，避免分钟取整刚好跨边界。"""
    return (datetime.datetime.now().astimezone() + datetime.timedelta(seconds=seconds)).isoformat()


def expected_reset(iso, now=None):
    """按产品规则从同一时刻计算两级倒计时，供 DOM 文本对照。"""
    remaining = datetime.datetime.fromisoformat(iso).timestamp() - (time.time() if now is None else now)
    if remaining <= 0:
        return "已到期"
    minutes = int(remaining // 60)
    if minutes >= 1440:
        return f"{minutes // 1440}d{(minutes % 1440) // 60}h"
    if minutes >= 60:
        return f"{minutes // 60}h{minutes % 60}m"
    return f"{minutes}m" if minutes else "<1m"


GEOMETRY = r"""
(() => {
  const layer = document.getElementById('route-lines').getBoundingClientRect();
  const router = document.querySelector('.router-emblem').getBoundingClientRect();
  const tall = document.getElementById('hud').dataset.layout === 'tall';
  const cards = [...document.querySelectorAll('.route-card')].filter(card => {
    const box = card.getBoundingClientRect();
    return box.width > 0 && box.height > 0 && box.bottom > 0 && box.top < innerHeight && box.right > 0 && box.left < innerWidth;
  });
  const paths = [...document.querySelectorAll('#route-lines path.branch')];
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const point = (path, end) => {
    const length = path.getTotalLength();
    const local = path.getPointAtLength(end ? length : 0);
    const matrix = path.getScreenCTM();
    return new DOMPoint(local.x, local.y).matrixTransform(matrix);
  };
  const samples = (path) => {
    const length = path.getTotalLength();
    const matrix = path.getScreenCTM();
    return Array.from({length: 25}, (_, index) => {
      const local = path.getPointAtLength(length * index / 24);
      return new DOMPoint(local.x, local.y).matrixTransform(matrix);
    });
  };
  return {
    layout: document.getElementById('hud').dataset.layout,
    tall,
    count: paths.length,
    visibleCards: cards.length,
    reducedMotion: reduced,
    layer: {left: layer.left, top: layer.top, right: layer.right, bottom: layer.bottom},
    paths: paths.map((path, index) => {
      const start = point(path, false);
      const end = point(path, true);
      const card = cards[index]?.getBoundingClientRect();
      const expectedX = tall ? router.left + router.width / 2 : router.right;
      const expectedY = tall ? router.bottom : router.top + router.height / 2;
      const base = path.previousElementSibling;
      const style = getComputedStyle(path);
      const baseStyle = base?.classList.contains('branch-base') ? getComputedStyle(base) : null;
      return {
        startDeltaX: start.x - expectedX,
        startDeltaY: start.y - expectedY,
        endDeltaX: card ? end.x - card.left : null,
        endDeltaY: card ? end.y - (card.top + card.height / 2) : null,
        outside: samples(path).some(sample => sample.x < layer.left - 1.5 || sample.x > layer.right + 1.5 || sample.y < layer.top - 1.5 || sample.y > layer.bottom + 1.5),
        running: path.classList.contains('running'),
        hasBase: path.classList.contains('running') && !!base?.classList.contains('branch-base') && base.getAttribute('d') === path.getAttribute('d'),
        baseFill: baseStyle ? baseStyle.fill : '',
        dash: style.strokeDasharray,
        animation: style.animationName,
      };
    }),
  };
})()
"""


def check_geometry(client, label):
    """读取实际路径屏幕坐标，确认起止点和底轨，不改 DOM。"""
    client.wait("document.querySelector('#route-lines path.branch') && document.querySelector('.route-card')")
    time.sleep(.12)
    measured = client.evaluate(GEOMETRY)
    failures = []
    if measured["count"] != measured["visibleCards"]:
        failures.append(f"{label}: branches={measured['count']} visibleCards={measured['visibleCards']}")
    for index, path in enumerate(measured["paths"]):
        if abs(path["startDeltaX"]) > 1.5 or abs(path["startDeltaY"]) > 1.5:
            failures.append(f"{label} path{index} start {path['startDeltaX']:.2f},{path['startDeltaY']:.2f}")
        if abs(path["endDeltaX"]) > 1.5 or abs(path["endDeltaY"]) > 1.5:
            failures.append(f"{label} path{index} end {path['endDeltaX']:.2f},{path['endDeltaY']:.2f}")
        if path["outside"]:
            failures.append(f"{label} path{index} outside route-lines")
        if path["running"] and not path["hasBase"]:
            failures.append(f"{label} path{index} missing branch-base")
        if path["running"] and path["baseFill"] != "none":
            failures.append(f"{label} path{index} branch-base fill={path['baseFill']}")
        expected_animation = "none" if measured["reducedMotion"] else "route-stream"
        if path["running"] and (path["dash"] in ("none", "0px", "") or path["animation"] != expected_animation):
            failures.append(f"{label} path{index} dash={path['dash']} animation={path['animation']}")
    return measured, failures


def set_size(client, width, height):
    """用产品 Tauri API 改窗口逻辑尺寸，并等待布局标记收敛。"""
    client.evaluate(f"window.__TAURI__.webviewWindow.getCurrentWebviewWindow().setSize(new window.__TAURI__.dpi.LogicalSize({width},{height}))")
    layout = "tall" if width / height < 1.25 or width < 680 else "wide"
    client.wait(f"innerWidth==={width} && innerHeight==={height} && document.getElementById('hud').dataset.layout==='{layout}'", timeout=8)


def select_session(client, value):
    """触发产品会话选择，不直接改额度 DOM。"""
    client.evaluate(f"(() => {{ const s=document.getElementById('session-select'); s.value={json.dumps(value)}; s.dispatchEvent(new Event('change')); }})()")


def quota_view(client):
    """读取当前额度项的账号、百分比、倒计时和提示。"""
    return client.evaluate(r"""
[...document.querySelectorAll('.quota-item')].map(item => ({
  account: item.querySelector('.quota-account').title,
  shown: item.querySelector('.quota-account').textContent,
  percent: item.querySelector('.quota-track').getAttribute('aria-valuenow'),
  reset: item.querySelector('.quota-reset').textContent,
  dateTime: item.querySelector('.quota-reset').dateTime,
  resetTitle: item.querySelector('.quota-reset').title,
  itemTitle: item.title,
  period: item.querySelector('.quota-period').textContent,
}))
""")


def startup_provider(client):
    """调用产品启动命令，读取当前会话卡的 provider。"""
    return client.evaluate(r"""
(async () => {
  const state = await window.__TAURI__.core.invoke('startup_state');
  return state.snapshot.sessions.flatMap(session => session.routes.map(card => ({
    session: session.id, account: card.account, model: card.model, provider: card.provider, state: card.state,
  })));
})()
""")


def main():
    """启动隔离实例，按场景切换临时网关并记录真实 DOM 结论。"""
    parser = argparse.ArgumentParser()
    parser.add_argument("--exe", default=str(ROOT / "src-tauri/target/release/magpie-hud.exe"))
    args = parser.parse_args()
    exe = Path(args.exe)
    assert exe.is_file(), f"missing executable: {exe}"
    scratch = Path(tempfile.mkdtemp(prefix="magpie-hud-quota-"))
    path = scratch / "usage.jsonl"
    path.write_text("", encoding="utf-8")
    appdata, localdata = scratch / "appdata", scratch / "localdata"
    started = datetime.datetime.now().astimezone()
    resets = {
        "day": reset_at(5 * 86400 + 2 * 3600 + 5),
        "hour": reset_at(5 * 3600 + 2 * 60 + 5),
        "minute": reset_at(32 * 60 + 5),
        "soon": reset_at(25),
        "expired": reset_at(-30),
    }
    grok_quota = quota("grok", GROK_ACCOUNT, 19, resets["minute"])
    gpt_quota = quota("codex", GPT_ACCOUNT, 40, resets["hour"])
    extra_quota = quota("codex", EXTRA_ACCOUNT, 77, resets["day"])
    long_grok = quota("grok", GROK_ACCOUNT, 19, resets["day"])
    long_gpt = quota("codex", GPT_ACCOUNT, 40, resets["hour"])
    missing_quota = quota("grok", "missing.time@example.com", 8, None)
    missing_quota["windows"][0]["resetsAt"] = None
    soon_quota = quota("grok", "soon.reset@example.com", 3, resets["soon"])
    expired_quota = quota("grok", "expired.reset@example.com", 2, resets["expired"])
    Gateway.state = {
        "seq": 1, "pid": 4100, "ids": [200],
        "routes": [live_route(200, "session-grok", [{"id": "grok", "who": GROK_ACCOUNT, "provider": "grok", "model": GROK_MODEL}], [{"id": "grok", "model": GROK_MODEL, "effort": "high", "done": False}])],
        "active": [active_request(1, 200, "session-grok")],
        "started": started.isoformat(), "quota_reads": 0, "quota_times": [],
        "quotas": [grok_quota, gpt_quota, extra_quota, soon_quota, expired_quota, missing_quota],
    }
    server = ThreadingHTTPServer(("127.0.0.1", 0), Gateway)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    config = appdata / "magpie-hud/config.json"
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({
        "usageJsonlPath": str(path), "magpieHost": f"127.0.0.1:{server.server_port}",
        "alwaysOnTop": False, "window": {"x": 40, "y": 40, "width": 1120, "height": 400},
    }), encoding="utf-8")
    port = free_port()
    profile = scratch / "profile"
    for directory in (appdata, localdata, scratch / "codex", profile, scratch / "webview", scratch / "exe"):
        directory.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(
        APPDATA=str(appdata), LOCALAPPDATA=str(localdata), CODEX_HOME=str(scratch / "codex"),
        USERPROFILE=str(profile),
        WEBVIEW2_USER_DATA_FOLDER=str(scratch / "webview"),
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=f"--remote-debugging-port={port} --remote-allow-origins=http://127.0.0.1:{port}",
    )
    checks, failures = [], []
    screenshots = {}
    copied = scratch / "exe/magpie-hud.exe"
    shutil.copy2(exe, copied)
    process = subprocess.Popen([str(copied)], cwd=scratch / "exe", env=env)
    client = None
    try:
        stage("launch")
        client = connect(port, process)
        stage("single-grok")
        client.wait("document.querySelectorAll('.quota-item').length===1 && document.querySelector('.quota-window strong')?.textContent==='19%'", timeout=20)
        initial = quota_view(client)
        if len(initial) == 1 and initial[0]["account"] == GROK_ACCOUNT and initial[0]["percent"] == "19" and initial[0]["reset"] == expected_reset(resets["minute"]) and initial[0]["dateTime"] == resets["minute"]:
            checks.append("single-grok-19")
        else:
            failures.append({"single-grok": initial})
        stage("repeat-snapshot")
        with Gateway.lock:
            Gateway.state["seq"] = 2
            Gateway.state["active"][0]["ms"] = 2400
        client.wait("document.getElementById('line1-text')?.textContent.includes('2.4s')", timeout=8)
        repeated = quota_view(client)
        if repeated == initial and client.evaluate("document.querySelectorAll('.quota-item').length") == 1:
            checks.append("repeat-snapshot-stable")
        else:
            failures.append({"repeat-snapshot": repeated})
        stage("routed-gpt-and-completed-grok")
        append(path, record(300, GROK_ACCOUNT, session="session-grok-done", model=GROK_MODEL, provider="grok", route_id=300, t=datetime.datetime.now().astimezone().isoformat()))
        with Gateway.lock:
            Gateway.state["seq"] = 3
            Gateway.state["ids"] = [400]
            Gateway.state["routes"] = [live_route(400, "session-gpt", [
                {"id": CANDIDATE_ID, "who": EXTRA_ACCOUNT, "provider": "codex", "model": OTHER_MODEL},
                {"id": CANDIDATE_ID, "who": GPT_ACCOUNT, "provider": "codex", "model": GPT_MODEL},
            ], [{"id": CANDIDATE_ID, "model": GPT_MODEL, "effort": "xhigh", "done": False}])]
            Gateway.state["active"] = [active_request(2, 400, "session-gpt")]
            Gateway.state["quotas"] = [long_gpt, long_grok, extra_quota, soon_quota, expired_quota, missing_quota]
            Gateway.state["seq"] = 3
        client.wait("document.querySelectorAll('.quota-item').length===2", timeout=12)
        both = quota_view(client)
        both_key = {(item["account"], item["percent"]) for item in both}
        if both_key == {(GPT_ACCOUNT, "40"), (GROK_ACCOUNT, "19")} and all(item["account"] != EXTRA_ACCOUNT for item in both):
            checks.append("gpt-and-grok-visible")
        else:
            failures.append({"gpt-and-grok": both})
        providers = startup_provider(client)
        gpt_cards = [card for card in providers if card["account"] == GPT_ACCOUNT]
        if gpt_cards and all(card["provider"] == "codex" for card in gpt_cards):
            checks.append("startup-gpt-provider-codex")
        else:
            failures.append({"startup-provider": providers})
        stage("session-switch")
        select_session(client, "session-gpt")
        client.wait("document.querySelectorAll('.quota-item').length===1 && document.querySelector('.quota-account')?.title===" + json.dumps(GPT_ACCOUNT))
        selected_gpt = quota_view(client)
        select_session(client, "session-grok-done")
        client.wait("document.querySelectorAll('.quota-item').length===1 && document.querySelector('.quota-account')?.title===" + json.dumps(GROK_ACCOUNT))
        selected_grok = quota_view(client)
        select_session(client, "*")
        client.wait("document.querySelectorAll('.quota-item').length===2")
        restored = quota_view(client)
        if [item["percent"] for item in selected_gpt] == ["40"] and [item["percent"] for item in selected_grok] == ["19"] and {(item["account"], item["percent"]) for item in restored} == both_key:
            checks.append("session-switch")
        else:
            failures.append({"session-switch": {"gpt": selected_gpt, "grok": selected_grok, "restored": restored}})
        stage("provider-order")
        with Gateway.lock:
            Gateway.state["seq"] = 4
            Gateway.state["ids"] = []
            Gateway.state["routes"] = []
            Gateway.state["active"] = []
            Gateway.state["quotas"] = [grok_quota]
            Gateway.state["seq"] = 4
        client.wait("document.querySelectorAll('.quota-item').length===1 && document.querySelector('.quota-account')?.title===" + json.dumps(GROK_ACCOUNT), timeout=8)
        single_order = quota_view(client)
        with Gateway.lock:
            Gateway.state["seq"] = 5
            Gateway.state["ids"] = [400]
            Gateway.state["routes"] = [live_route(400, "session-gpt", [
                {"id": CANDIDATE_ID, "who": GPT_ACCOUNT, "provider": "codex", "model": GPT_MODEL},
            ], [{"id": CANDIDATE_ID, "model": GPT_MODEL, "effort": "xhigh", "done": False}])]
            Gateway.state["active"] = [active_request(3, 400, "session-gpt")]
            Gateway.state["quotas"] = [gpt_quota, grok_quota]
            Gateway.state["seq"] = 5
        started_order = time.monotonic()
        client.wait("document.querySelectorAll('.quota-item').length===2", timeout=8)
        elapsed = time.monotonic() - started_order
        ordered = quota_view(client)
        expected_order = [item["account"] for item in restored]
        if elapsed < 55 and [item["account"] for item in single_order] == [GROK_ACCOUNT] and [item["account"] for item in ordered] == expected_order:
            checks.append("stable-order-without-minute-wait")
        else:
            failures.append({"order": {"elapsed": elapsed, "single": single_order, "ordered": ordered}})
        stage("reset-duration")
        select_session(client, "*")
        with Gateway.lock:
            resets["soon"] = reset_at(25)
            soon_quota["windows"][0]["resetsAt"] = resets["soon"]
            Gateway.state["seq"] = 6
            Gateway.state["ids"] = [501, 502, 503]
            Gateway.state["routes"] = [
                live_route(501, "session-soon", [{"id": "soon", "who": "soon.reset@example.com", "provider": "grok", "model": GROK_MODEL}], [{"id": "soon", "model": GROK_MODEL, "effort": "low", "done": False}]),
                live_route(502, "session-expired", [{"id": "expired", "who": "expired.reset@example.com", "provider": "grok", "model": GROK_MODEL}], [{"id": "expired", "model": GROK_MODEL, "effort": "low", "done": False}]),
                live_route(503, "session-missing", [{"id": "missing", "who": "missing.time@example.com", "provider": "grok", "model": GROK_MODEL}], [{"id": "missing", "model": GROK_MODEL, "effort": "low", "done": False}]),
            ]
            Gateway.state["active"] = [active_request(21, 501, "session-soon"), active_request(22, 502, "session-expired"), active_request(23, 503, "session-missing")]
            Gateway.state["quotas"] = [long_grok, soon_quota, expired_quota, missing_quota]
            Gateway.state["seq"] = 6
        client.wait("document.querySelectorAll('.quota-item').length===3 && document.querySelector('.quota-account')?.title==='soon.reset@example.com'", timeout=8)
        observed_at = time.time()
        durations = {item["account"]: item for item in quota_view(client)}
        select_session(client, "session-grok-done")
        client.wait("document.querySelectorAll('.quota-item').length===1 && document.querySelector('.quota-account')?.title===" + json.dumps(GROK_ACCOUNT), timeout=8)
        durations.update({item["account"]: item for item in quota_view(client)})
        expected = {
            GROK_ACCOUNT: (resets["day"], expected_reset(resets["day"], observed_at)),
            "soon.reset@example.com": (resets["soon"], expected_reset(resets["soon"], observed_at)),
            "expired.reset@example.com": (resets["expired"], "已到期"),
            "missing.time@example.com": ("", "—"),
        }
        duration_ok = True
        for account, (iso, text) in expected.items():
            item = durations.get(account)
            if not item or item["dateTime"] != iso or item["reset"] != text:
                duration_ok = False
            elif iso:
                stamp = client.evaluate("(iso => new Date(iso).toLocaleString('zh-CN', {hour12:false}))(" + json.dumps(iso) + ")")
                if item["resetTitle"] != f"{stamp} 重置" or item["itemTitle"].split("\n")[3] != f"{stamp} 重置":
                    duration_ok = False
            elif item["resetTitle"] != "重置时间未提供" or "重置时间未提供" not in item["itemTitle"]:
                duration_ok = False
        if duration_ok:
            checks.append("reset-duration")
        else:
            failures.append({"reset-duration": durations, "expected": {key: value[1] for key, value in expected.items()}})
        stage("geometry")
        select_session(client, "*")
        with Gateway.lock:
            Gateway.state["quotas"] = [gpt_quota, grok_quota]
            Gateway.state["seq"] = 7
            Gateway.state["ids"] = [400]
            Gateway.state["routes"] = [live_route(400, "session-gpt", [
                {"id": CANDIDATE_ID, "who": GPT_ACCOUNT, "provider": "codex", "model": GPT_MODEL},
            ], [{"id": CANDIDATE_ID, "model": GPT_MODEL, "effort": "xhigh", "done": False}])]
            Gateway.state["active"] = [active_request(4, 400, "session-gpt")]
            Gateway.state["seq"] = 7
        client.wait("document.querySelector('.route-card.running')", timeout=8)
        geometry = {}
        for label, size in (("wide", (690, 340)), ("normal", (1120, 400)), ("tall", (420, 760)), ("narrow", (360, 640))):
            stage(f"geometry-{label}")
            set_size(client, *size)
            measured, geometry_failures = check_geometry(client, label)
            geometry[label] = measured
            failures.extend(geometry_failures)
            if label in ("wide", "tall") and not geometry_failures:
                screenshots[label] = screenshot(client, scratch, f"hud-quota-connections-{label}.png")
        if not any(str(item).startswith(("wide", "normal", "tall", "narrow")) for item in failures):
            checks.append("route-geometry")
        report = {"passed": not failures, "checks": checks, "failures": failures, "screenshots": screenshots, "scratch": str(scratch)}
        print(json.dumps(report, ensure_ascii=False), flush=True)
        return 0 if not failures else 1
    except Exception as error:
        report = {"passed": False, "checks": checks, "failures": failures + [{"exception": f"{type(error).__name__}: {error}"}], "screenshots": screenshots, "scratch": str(scratch)}
        print(json.dumps(report, ensure_ascii=False), flush=True)
        return 1
    finally:
        if client:
            try:
                client.evaluate("setTimeout(()=>window.__TAURI__.webviewWindow.getCurrentWebviewWindow().close(),50)")
            except (websocket.WebSocketException, OSError, AssertionError):
                pass
            client.socket.close()
        if process.poll() is None:
            try:
                process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=5)
        server.shutdown()
        server.server_close()
        thread.join(timeout=3)


if __name__ == "__main__":
    raise SystemExit(main())
