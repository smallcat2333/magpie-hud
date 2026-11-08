'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

function panel(options = {}) {
  const elements = new Map(), handlers = {}, calls = [], timers = new Map();
  let now = options.now ?? 100_000, timerId = 0;
  // 模拟 UTC 客户端；服务器时区另外从 HudData 传入。
  class Clock extends Date { static now() { return now; } getHours() { return this.getUTCHours(); } }
  class Element {
    constructor() { this.children = []; this.hidden = false; }
    append(...values) { this.children.push(...values); }
    replaceChildren(...values) { this.children = values; }
    addEventListener() {}
    setAttribute() {}
    getContext() { return {}; }
  }
  const element = name => {
    if (!elements.has(name)) elements.set(name, new Element());
    return elements.get(name);
  };
  const window = {
    addEventListener: (name, fn) => { handlers[name] = fn; },
    HudData: {
      preview: false, generation: 1,
      invoke(command, args) {
        assert.equal(command, 'query_hourly_costs');
        return new Promise((resolve, reject) => calls.push({ args, resolve, reject }));
      },
      listen(event, fn) { handlers[event] = fn; return Promise.resolve(() => {}); },
      now: () => now,
      timeZone: () => options.zone || 'UTC',
      today: () => options.today || new Intl.DateTimeFormat('en-CA', { timeZone: 'UTC', year: 'numeric', month: '2-digit', day: '2-digit' }).format(new Date()),
    },
  };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, '../ui/hourly.js'), 'utf8'), {
    window, Date: Clock, Intl, console,
    document: { getElementById: element, querySelector: element, createElement: () => new Element() },
    ResizeObserver: class { observe() {} },
    setTimeout: (fn, ms) => { const id = ++timerId; timers.set(id, { fn, ms, at: now + ms }); return id; },
    clearTimeout: id => timers.delete(id),
  });
  const today = window.HudData.today();
  return {
    calls, today,
    update(sourceKey = 'A', date = today) { handlers['hud-hourly-update']({ detail: {
      date, sourceKey, scope: sourceKey || '全部', hourly: Array.from({ length: 24 }, () => ({ tokens: 100, cost: sourceKey ? null : 9 })),
    } }); },
    row(hour = 0) { return element('#hourly-table tbody').children[hour].children.map(cell => cell.textContent).join('|'); },
    title: () => element('hourly-plot').title,
    tick(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.ms > 0 && timer.at <= now) { timers.delete(id); timer.fn(); }
      }
    },
  };
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const prices = (date, cost, partial = false) => ({ date, fetchFailed: false, hours: Array.from({ length: 24 }, () => ({
  cost, requests: 5, priced: partial ? 3 : 5, unpriced: partial ? 1 : 0, unmatched: partial ? 1 : 0,
})) });

(async () => {
  const app = panel();
  for (let i = 0; i < 20; i++) app.update();
  assert.equal(app.calls.length, 1, '连续快照复用在途查询');
  app.calls[0].resolve(prices(app.today, 1.25, true)); await flush();
  assert.match(app.row(), /1\.250000（已知金额）；已定价 3\/5/);
  app.update();
  assert.equal(app.calls.length, 1, '完成后缓存命中不重复调用');
  app.tick(60_010); await flush();
  assert.equal(app.calls.length, 2);
  app.calls[1].reject(new Error('offline')); await flush();
  assert.match(app.row(), /1\.250000/, '更新失败保留原来源费用');
  assert.match(app.title(), /更新失败/);
  app.update('B'); app.update('C');
  app.calls[3].resolve(prices(app.today, 3)); await flush();
  app.calls[2].resolve(prices(app.today, 999)); await flush();
  assert.match(app.row(), /3\.000000/, '晚到的 B 不能覆盖 C');
  app.update('B'); assert.match(app.row(), /999\.000000/, '切回 B 立即使用自己的缓存');
  app.update(null); assert.match(app.row(), /9\.000000/, '全局继续使用网关原序列');
  const count = app.calls.length;
  app.tick(300_000); assert.equal(app.calls.length, count, '全局不进行来源费用轮询');
  app.update('history', '2025-01-01');
  app.calls.at(-1).resolve(prices('2025-01-01', 0)); await flush();
  app.tick(299_999); assert.equal(app.calls.length, count + 1, '历史缓存五分钟');
  app.tick(1); assert.equal(app.calls.length, count + 2);
  const remote = panel({ now: Date.parse('2026-11-08T08:00:00Z'), today: '2026-11-08', zone: 'Asia/Shanghai' });
  remote.update(null);
  assert.match(remote.row(15), /100/, 'UTC 客户端 8 点仍显示上海服务端 15 点已有数据');
  assert.match(remote.row(16), /100/, '采集端当前小时可见');
  assert.doesNotMatch(remote.row(17), /100/, '采集端未来小时留空');
  console.log('hourly-costs: cache, in-flight dedupe, scope isolation, partial coverage, failure retention, history TTL and remote timezone passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
