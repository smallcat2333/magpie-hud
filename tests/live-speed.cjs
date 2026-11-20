'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const window = {};
const script = path.join(__dirname, '../ui/live-speed.js');
vm.runInNewContext(fs.readFileSync(script, 'utf8'), { window }, { filename: script });
const { cardSpeed, aggregate, createAverage, relativeChange } = window.hudLiveSpeed;

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

const { createTrend, recentTrend } = window.hudLiveSpeed;
const minute = 60_000;
const plain = points => structuredClone(points);

// 打开时立即从磁盘生成 30 格，输出从首字开始，并发从请求开始。
const requests = [
  { session: 'a', start: 0, firstToken: minute, end: 2 * minute, output: 120 },
  { session: 'a', start: 30_000, end: 90_000, output: 60 },
  { session: 'b', start: 30_000, end: 90_000, output: 0 },
];
const initial = plain(recentTrend(requests, new Map(), 2 * minute + 1, true));
assert.equal(initial.length, 30);
assert.deepEqual(initial.slice(-2), [
  { end: minute, tps: 0.5, concurrency: 1.5 },
  { end: 2 * minute, tps: 2.5, concurrency: 1.5 },
]);
assert.deepEqual(initial.at(-3), { end: 0, tps: 0, concurrency: 0 });
// 超长请求仍回填，但只遍历图中的 30 格。
const long = recentTrend([{ session: 'a', start: 0, end: 90 * minute, output: 5400 }], new Map(), 90 * minute, true);
assert.equal(long[0].end, 61 * minute);
assert.equal(long.at(-1).tps, 1);

// 零值也入图，空闲超过 30 分钟后仍逐分钟推移。
const trend = createTrend(30_000);
trend.record(30_000, 100, 2);
assert.equal(trend.record(minute, 0, 0), true);
assert.equal(trend.points.has(minute), false);
for (let i = 2; i <= 42; i++) assert.equal(trend.record(i * minute), true);
assert.equal(trend.points.size, 30);
const idle = plain(recentTrend([], trend.points, 42 * minute, false));
assert.deepEqual(idle[0], { end: 13 * minute, tps: 0, concurrency: 0 });
assert.deepEqual(idle.at(-1), { end: 42 * minute, tps: 0, concurrency: 0 });

// 睡眠时不把旧读数延伸到间隔里，回来后的完整分钟继续记。
assert.equal(trend.record(75 * minute, 40, 2), true);
assert.equal(trend.points.size, 0);
assert.deepEqual(plain(recentTrend([], trend.points, 75 * minute, false)).at(-1),
  { end: 75 * minute, tps: null, concurrency: null });
assert.equal(trend.record(76 * minute), true);
assert.deepEqual({ ...trend.points.get(76 * minute) }, { tps: 40, concurrency: 2 });
const weighted = createTrend(0);
weighted.record(0, 40, 2);
assert.equal(weighted.record(30_000, 20, 1), false);
weighted.record(60_005);
assert.deepEqual({ ...weighted.points.get(minute) }, { tps: 30, concurrency: 1.5 });
weighted.record(2 * minute);
assert.deepEqual({ ...weighted.points.get(2 * minute) }, { tps: 20, concurrency: 1 });

console.log('live-speed tests passed');
