'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const flush = () => new Promise(resolve => setImmediate(resolve));

// 运行真实履历脚本；用虚拟时钟检查一分钟内实际发出的 IPC 数量。
function panel({ slow = false } = {}) {
  const cards = [];
  let document;
  class Element {
    constructor() { this.children = []; this.dataset = {}; this.style = {}; this.attributes = {}; this.handlers = {}; this.hidden = false; this.isConnected = true; this.offsetWidth = 400; this.offsetHeight = 200; }
    append(...children) { this.children.push(...children); }
    replaceChildren(...children) { this.children = children; }
    setAttribute(name, value) { this.attributes[name] = value; }
    removeAttribute(name) { delete this.attributes[name]; }
    addEventListener(name, fn) { this.handlers[name] = fn; }
    querySelectorAll() { return this === routes ? cards : []; }
    closest(selector) { return selector.includes('.route-card') && this.card ? this : null; }
    contains(other) { return other === this || this.children.some(child => child.contains?.(other)); }
    getBoundingClientRect() { return { top: 40, bottom: 80, right: 800 }; }
    focus() { document.activeElement = this; }
  }
  const routes = new Element();
  document = { hidden: false, activeElement: null, body: new Element(), handlers: {},
    createElement: () => new Element(), createDocumentFragment: () => new Element(), getElementById: () => routes,
    addEventListener(name, fn) { this.handlers[name] = fn; } };
  const calls = [], timers = new Map();
  let clock = 0, timerId = 0;
  const data = { rows: [], warnings: [], bytesScanned: 100, rowLimited: false };
  const window = { addEventListener() {}, hudRedact: { scrub: value => value }, HudData: {
    preview: false,
    invoke(command, args) {
      assert.equal(command, 'query_route_history');
      if (!slow) { calls.push({ args }); return Promise.resolve(data); }
      return new Promise((resolve, reject) => calls.push({ args, resolve, reject }));
    },
    listen() { return Promise.resolve(() => {}); },
  } };
  const context = vm.createContext({ window, document, Element, console, innerWidth: 1120, innerHeight: 800,
    performance: { now: () => clock },
    setTimeout(fn, delay) { const id = ++timerId; timers.set(id, { at: clock + delay, fn }); return id; },
    clearTimeout(id) { timers.delete(id); },
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../ui/history.js'), 'utf8'), context);
  const overlay = document.body.children[0];
  function open(session = 'example-session') {
    let card = cards.find(card => card.session === session);
    if (!card) {
      card = new Element(); card.card = true; card.session = session;
      cards.push(card);
      window.hudHistory.attach(card, { session, label: 'Example Session', source: 'Example Agent' });
    }
    routes.handlers.keydown({ target: card, altKey: true, key: 'ArrowDown', preventDefault() {} });
  }
  function action(label) {
    const find = node => node.attributes?.['aria-label'] === label ? node : node.children.map(find).find(Boolean);
    const button = find(overlay);
    assert.ok(button, label);
    button.handlers.click();
  }
  return { calls, timers, open, action,
    close() { document.handlers.keydown({ key: 'Escape', preventDefault() {} }); },
    visibility(hidden) { document.hidden = hidden; document.handlers.visibilitychange(); },
    async advance(ms) {
      const target = clock + ms;
      while (true) {
        const next = [...timers].filter(([, timer]) => timer.at <= target).sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        timers.delete(next[0]); clock = next[1].at; next[1].fn(); await flush();
      }
      clock = target; await flush();
    },
  };
}

(async () => {
  const automatic = panel();
  automatic.open(); await flush();
  await automatic.advance(60_000);
  assert.equal(automatic.calls.length, 7, '打开一分钟只应有初次查询和 6 次自动刷新');
  automatic.close(); automatic.open(); await flush();
  assert.equal(automatic.calls.length, 7, '重复打开同一范围复用近期前端读数');
  automatic.action('刷新履历'); await flush();
  assert.equal(automatic.calls.length, 8, '手动刷新绕过前端缓存');
  automatic.action('展开最近 200 组输入'); await flush();
  assert.equal(automatic.calls.at(-1).args.limit, 200, '展开仍立即读取更大范围');
  automatic.visibility(true);
  assert.equal(automatic.timers.size, 0, '隐藏页面停止刷新计时器');
  const hiddenCount = automatic.calls.length;
  await automatic.advance(60_000);
  assert.equal(automatic.calls.length, hiddenCount);
  automatic.visibility(false); await flush();
  assert.equal(automatic.calls.length, hiddenCount + 1, '返回可见页面立即更新过期读数');
  automatic.close();
  const closedCount = automatic.calls.length;
  await automatic.advance(60_000);
  assert.equal(automatic.calls.length, closedCount, '关闭浮层后停止刷新');

  const pending = panel({ slow: true });
  pending.open(); await flush();
  pending.action('刷新履历'); pending.close(); pending.open(); await flush();
  assert.equal(pending.calls.length, 1, '慢查询、手动刷新和重开都复用在途请求');
  await pending.advance(60_000);
  assert.equal(pending.calls.length, 1, '查询结束前不叠加下一轮');
  pending.calls[0].resolve({ rows: [], warnings: [] }); await flush();
  await pending.advance(10_000);
  assert.equal(pending.calls.length, 2, '慢查询结束后按正常节拍继续');
  pending.visibility(true);
  pending.calls[1].resolve({ rows: [], warnings: [] }); await flush();
  assert.equal(pending.timers.size, 0, '后台完成的查询不能重新启动隐藏页计时器');
  console.log('History refresh: 7 queries/minute, cache, manual refresh, expansion, visibility and pending requests passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
