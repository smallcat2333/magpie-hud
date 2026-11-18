'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

// 执行实际面板脚本，只替换 DOM、时钟和 IPC；不复制生产状态机。
function panel() {
  class Element {
    constructor() { this.children = []; this.hidden = false; this.dataset = {}; this.style = {}; this.handlers = {}; this.clientWidth = 220; this.clientHeight = 120; }
    append(...children) {
      for (const child of children) child.parent = this;
      this.children.push(...children);
    }
    replaceChildren(...children) { this.children = children; }
    get firstElementChild() { return this.children[0]; }
    get lastElementChild() { return this.children[this.children.length - 1]; }
    setAttribute() {}
    removeAttribute() {}
    addEventListener(name, handler) { this.handlers[name] = handler; }
    querySelectorAll(selector) { return selector === '.model-usage-cell' ? this.children.filter(child => child.dataset.model) : []; }
    querySelector() { return null; }
    remove() { this.parent.children = this.parent.children.filter(child => child !== this); }
    getBoundingClientRect() { return { top: 0, bottom: 1000, width: 220, height: 120 }; }
  }
  const elements = new Map();
  const element = id => {
    if (!elements.has(id)) elements.set(id, new Element());
    return elements.get(id);
  };
  const cost = new Element(); cost.dataset.modelMode = 'cost';
  const tokens = new Element(); tokens.dataset.modelMode = 'tokens';
  element('model-usage').querySelectorAll = () => [cost, tokens];
  const calls = [], intervals = [];
  let clock = 100_000;
  class Clock extends Date { static now() { return clock; } }
  const window = {
    addEventListener() {},
    HudData: {
      preview: false,
      invoke(command, args) {
        assert.equal(command, 'query_model_costs');
        return new Promise((resolve, reject) => calls.push({ args, resolve, reject }));
      },
      listen() { return Promise.resolve(() => {}); },
    },
  };
  const context = vm.createContext({ window, Date: Clock, console, innerHeight: 1000, getComputedStyle: () => ({ rowGap: '0', paddingBottom: '0' }),
    document: { getElementById: element, querySelector: element, createElement: () => new Element(), addEventListener() {} },
    ResizeObserver: class { observe() {} }, requestAnimationFrame: () => 1, cancelAnimationFrame() {},
    setTimeout: () => 1, clearTimeout() {}, setInterval: (fn, ms) => { intervals.push({ fn, ms }); return 1; },
  });
  const helper = path.join(__dirname, '../ui/model-costs.js');
  if (fs.existsSync(helper)) vm.runInContext(fs.readFileSync(helper, 'utf8'), context);
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../ui/model-usage.js'), 'utf8'), context);
  const update = (revision, session = null, extra = {}, rows = []) => window.hudModelUsage.update(rows, '测试范围', {
    date: '2026-11-06', revision, session, sourceKey: null, ...extra,
  });
  return { calls, update, elements, clickCost: () => cost.handlers.click(), clickTokens: () => tokens.handlers.click(),
    tick(ms) { clock += ms; for (const { fn } of intervals) fn(); } };
}
const flush = () => new Promise(resolve => setImmediate(resolve));
const result = (revision = 1, cost = 1.25, date = '2026-11-06') => ({ date, ledgerRevision: revision,
  rows: [{ model: 'Example Model', effort: 'high', cost, priced: 1, unpriced: 0, unmatched: 0 }], warnings: [] });
const summary = app => app.elements.get('model-usage-summary').textContent;
const modelColors = app => Object.fromEntries(app.elements.get('model-usage-plot').children
  .filter(cell => cell.dataset.model).map(cell => [cell.dataset.model, cell.style.backgroundColor]));

// 首次出现的顺序、历史范围和累计出现过的模型数都不影响颜色。
{
  const model = (name, tokens = 1000) => ({ model: name, tokens, requests: 1 });
  const today = panel();
  const historical = panel();
  const first = model('gpt-6.1-sol');
  const second = model('gpt-6.2-sol');
  today.update(1, null, {}, [first, model(second.model, 2000)]);
  const colors = modelColors(today);
  historical.update(1, null, { historical: true }, [model(first.model, 2000), second]);
  assert.deepEqual(modelColors(historical), colors);
  assert.notEqual(colors[first.model], colors[second.model]);
  const hue = color => Number(color.match(/^oklch\(\d+% 0\.14 (\d+)\)$/)[1]);
  assert.ok(Math.abs(hue(colors[first.model]) - hue(colors[second.model])) <= 16);

  const newModels = new Set();
  for (let i = 1; i <= 12; i++) {
    const name = `quasar-${i}-pro`;
    today.update(i + 1, null, {}, [model(name)]);
    const color = modelColors(today)[name];
    assert.match(color, /^oklch\(/);
    newModels.add(color);
  }
  assert.ok(newModels.size > 10);
}

(async () => {
  const active = panel();
  for (let revision = 1; revision <= 20; revision++) active.update(revision);
  await flush();
  assert.equal(active.calls.length, 1, '20 次账本追加只能产生一个同范围在途查询');
  active.calls[0].resolve(result(1));
  await flush();
  active.clickCost(); await flush();
  assert.match(summary(active), /1\.25/, 'revision 已增长仍展示精确匹配的旧行费用');
  active.update(21); await flush();
  assert.match(summary(active), /1\.25/, '追加记录不清空已有费用');
  active.tick(29_999); await flush();
  assert.equal(active.calls.length, 1, '完成后 30 秒以内不重新查询');
  active.tick(1); await flush();
  assert.equal(active.calls.length, 2);
  active.clickTokens(); active.clickCost();
  assert.match(summary(active), /1\.25/, '后台查询尚未结束，切回费用也应立即显示上次结果');
  active.update(22); active.tick(30_000); await flush();
  assert.equal(active.calls.length, 2, '慢查询运行期间节拍不会叠加请求');
  active.calls[1].reject(new Error('temporary failure')); await flush();
  assert.match(summary(active), /1\.25/, '查询失败保留已有费用');
  active.clickCost(); await flush();
  assert.equal(active.calls.length, 3, '用户点击费用可以立即重试失败请求');
  active.calls[2].resolve(result(22, 2.5)); await flush();
  assert.match(summary(active), /2\.50/);

  const switching = panel();
  switching.update(1, 'A'); await flush();
  switching.update(1, 'B'); await flush();
  switching.update(20, 'A'); await flush();
  assert.equal(switching.calls.length, 2, '切走再切回同范围复用在途查询');
  switching.calls[1].resolve(result(1, 99)); await flush();
  switching.clickCost(); await flush();
  assert.doesNotMatch(summary(switching), /99/, '其他范围的慢响应不能污染当前范围');
  switching.calls[0].resolve(result(1, 3)); await flush();
  assert.match(summary(switching), /3\.00/);
  switching.update(2, 'B'); await flush();
  assert.match(summary(switching), /99\.00/, '切回已完成范围可直接展示缓存');
  assert.equal(switching.calls.length, 2);

  const history = panel();
  history.update(0, null, { date: '2026-11-05', historical: true, sourceKey: 'example' }); await flush();
  assert.equal(history.calls[0].args.date, '2026-11-05');
  assert.equal(history.calls[0].args.sourceKey, 'example');
  history.calls[0].resolve(result(0, 5, '2026-11-06')); await flush();
  history.clickCost(); await flush();
  assert.doesNotMatch(summary(history), /5\.00/, '不同日期的结果仍然拒绝');
  history.calls[1].resolve(result(0, 6, '2026-11-05')); await flush();
  assert.match(summary(history), /6\.00/);

  console.log('Model cost loading: concurrency, revisions, cooldown, scope switches, retry and date isolation passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
