'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const window = {};
const script = path.join(__dirname, '../ui/live-speed.js');
vm.runInNewContext(fs.readFileSync(script, 'utf8'), { window }, { filename: script });
const { cardSpeed, aggregate } = window.hudLiveSpeed;

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

console.log('live-speed tests passed');
