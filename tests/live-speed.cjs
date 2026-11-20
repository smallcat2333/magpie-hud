'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const window = {};
const script = path.join(__dirname, '../ui/live-speed.js');
vm.runInNewContext(fs.readFileSync(script, 'utf8'), { window }, { filename: script });
const { cardSpeed, aggregate, createAverage, createTrend, relativeChange } = window.hudLiveSpeed;

const running = { liveTokensPerSecond: 82.3, liveOutputTokens: 1234, tokensPerSecond: 42.6 };
const fallback = { value: 42.6, live: false, text: '42.6 TPS' };
assert.deepEqual({ ...cardSpeed(running, true) }, { value: 82.3, live: true, text: '≈82 TPS' });
assert.deepEqual({ ...cardSpeed(running, false) }, fallback);
assert.deepEqual({ ...cardSpeed({ tokensPerSecond: 42.6 }, true) }, fallback);
assert.deepEqual({ ...cardSpeed({ liveTokensPerSecond: 0 }, true) }, { value: 0, live: true, text: '≈0 TPS' });
assert.deepEqual({ ...cardSpeed({ liveTokensPerSecond: 15.5 }, true) }, { value: 15.5, live: true, text: '≈16 TPS' });
assert.deepEqual({ ...cardSpeed({}, true) }, { value: null, live: false, text: '-- TPS' });
assert.deepEqual({ ...cardSpeed({ liveTokensPerSecond: 15 }, false) }, { value: null, live: false, text: '-- TPS' });

const invalid = [NaN, Infinity, -Infinity, -1, '12.5', null, undefined];
for (const value of invalid) {
  assert.deepEqual({ ...cardSpeed({ ...running, liveTokensPerSecond: value }, true) }, fallback);
  assert.deepEqual({ ...cardSpeed({ tokensPerSecond: value }, false) }, { value: null, live: false, text: '-- TPS' });
}

assert.deepEqual({ ...aggregate([]) }, { tokensPerSecond: 0, streams: 0 });
assert.deepEqual({ ...aggregate([{}, { tokensPerSecond: 42.6 }]) }, { tokensPerSecond: 0, streams: 0 });
assert.deepEqual({ ...aggregate([
  { liveTokensPerSecond: 41.25 }, { liveTokensPerSecond: 11.5 }, { liveTokensPerSecond: 0 },
  { tokensPerSecond: 999 }, {}, ...invalid.map(liveTokensPerSecond => ({ liveTokensPerSecond })),
]) }, { tokensPerSecond: 52.75, streams: 3 });

// 非等间隔采样按实际持续时间计权（默认 15 秒窗口），中途请求结束照常计入空闲时段。
const windowed = createAverage(100);
assert.equal(windowed.deadline, 15_100);
assert.equal(windowed.record(100, 100), undefined);
assert.equal(windowed.record(5_100, 200), undefined);
assert.equal(windowed.record(10_100, 0), undefined);
assert.equal(windowed.record(15_099), undefined);
assert.equal(windowed.record(15_100), 100);
assert.equal(windowed.record(30_100), 0);

// 窗口边界与延迟定时器都不能把新采样应用到前一个窗口。
const delayed = createAverage(0);
delayed.record(0, 80);
assert.equal(delayed.record(18_000, 140), 80);
assert.equal(delayed.record(30_000), 128);
assert.equal(delayed.record(78_000, null), 140);
assert.equal(delayed.deadline, 90_000);
assert.equal(delayed.record(90_000), 140 * 3_000 / 15_000);
assert.equal(delayed.record(105_000), null);

for (const [previous, current, expected] of [
  [null, 100, 0], [100, 100, 0], [100, 125, .25], [100, 50, -.5],
  [100, 0, -1], [100, 200, 1], [100, 450, 1], [0, 0, 0], [0, 100, 1],
]) {
  assert.equal(relativeChange(previous, current), expected);
}

// 趋势图按分钟统计，最多保留最近 30 分钟（30 个点）。
const trend = createTrend(100, 10_000);
assert.equal(trend.deadline, 60_100);
assert.equal(trend.record(100, 150, 2), undefined);
assert.equal(trend.record(30_100, 50, 1), undefined);
assert.deepEqual({ ...trend.record(60_100, null, 1) }, { end: 70_000, tps: 100, concurrency: 1.5 });
assert.deepEqual({ ...trend.record(120_100, 0, 0) }, { end: 130_000, tps: null, concurrency: 1 });
assert.deepEqual({ ...trend.record(180_100, null, null) }, { end: 190_000, tps: 0, concurrency: 0 });
assert.deepEqual({ ...trend.record(300_100, 90, 1) }, { end: 310_000, tps: null, concurrency: null });
assert.deepEqual({ ...trend.points.at(-2) }, { end: 250_000, tps: null, concurrency: null });
for (let i = 6; i <= 40; i++) trend.record(100 + i * 60_000);
assert.equal(trend.points.length, 30);
assert.equal(trend.points[0].end, 670_000);
assert.deepEqual({ ...trend.points.at(-1) }, { end: 2_410_000, tps: 90, concurrency: 1 });

console.log('live-speed tests passed');
