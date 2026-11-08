'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const handlers = new Map();
let localNow = Date.parse('2026-11-08T08:00:00Z');
class Clock extends Date { static now() { return localNow; } }
const state = {
  timezone: 'Asia/Shanghai', serverNow: '2026-11-08T09:00:00Z',
  snapshot: { date: '2026-11-08' },
};
const window = { __TAURI__: {
  core: { invoke: async command => { assert.equal(command, 'startup_state'); return state; } },
  event: { listen: async (name, handler) => { handlers.set(name, handler); return () => {}; } },
} };
vm.runInNewContext(fs.readFileSync(require('node:path').join(__dirname, '../ui/data-client.js'), 'utf8'), {
  window, Date: Clock, URLSearchParams, location: { search: '' },
  document: { documentElement: { dataset: {} } }, console,
});
(async () => {
  const data = window.HudData;
  await data.startup();
  assert.equal(data.timeZone(), 'Asia/Shanghai');
  assert.equal(data.now(), Date.parse(state.serverNow));
  localNow += 1000;
  assert.equal(data.now(), Date.parse(state.serverNow) + 1000, '校准后时钟继续前进');
  handlers.get('connection-status')({ payload: { connected: true, timezone: 'UTC', serverNow: '2026-11-08T10:00:00Z' } });
  assert.equal(data.now(), Date.parse('2026-11-08T10:00:00Z'));
  assert.equal(data.timeZone(), 'UTC');
  handlers.get('source-change')({ payload: { mode: 'local', generation: 2, timezone: 'Asia/Tokyo' } });
  assert.equal(data.now(), localNow, '换源清除上一服务器的时钟偏差');
  assert.equal(data.today(), '', '换源不沿用上一数据源的日期');
  assert.equal(data.serverNowKnown, false);
  assert.equal(data.timeZone(), 'Asia/Tokyo');
  console.log('Data client clock: startup, connection updates, ticking and source isolation passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
