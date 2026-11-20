'use strict';

(() => {
  const validSpeed = value => Number.isFinite(value) && value >= 0;

  // 有实时估算时显示本轮速度，否则沿用已完成请求的读数。
  function cardSpeed(card, isLive) {
    if (isLive && validSpeed(card.liveTokensPerSecond)) {
      const value = card.liveTokensPerSecond;
      return { value, live: true, text: `≈${Math.round(value)} TPS` };
    }
    const value = validSpeed(card.tokensPerSecond) ? card.tokensPerSecond : null;
    return { value, live: false, text: value === null ? '-- TPS' : `${value.toFixed(1)} TPS` };
  }

  // 调用方按会话范围和活动状态筛选，缺少估算的卡片不计入路数。
  function aggregate(cards) {
    let tokensPerSecond = 0;
    let streams = 0;
    for (const { liveTokensPerSecond } of cards) {
      if (!validSpeed(liveTokensPerSecond)) continue;
      tokensPerSecond += liveTokensPerSecond;
      streams++;
    }
    return { tokensPerSecond, streams };
  }

  const speedInterval = 15_000;
  const trendInterval = 60_000;
  const trendCapacity = 30;

  // 每个采样值延续到下一次采样，空闲时段以 0 计。
  function createAverage(startedAt, interval = speedInterval) {
    let deadline = startedAt + interval;
    let updatedAt = startedAt;
    let rate = null;
    let weighted = 0;
    let hasSample = false;

    return {
      get deadline() { return deadline; },
      record(now, nextRate = rate) {
        const elapsed = Math.min(now, deadline) - updatedAt;
        weighted += (rate ?? 0) * elapsed;
        hasSample ||= rate !== null && elapsed > 0;
        let average;
        if (now >= deadline) {
          // 休眠或后台限频跨过多个周期时，只展示最近一个完整周期。
          const skipped = Math.floor((now - deadline) / interval);
          average = skipped ? rate : hasSample ? weighted / interval : null;
          deadline += (skipped + 1) * interval;
          const remainder = now - (deadline - interval);
          weighted = (rate ?? 0) * remainder;
          hasSample = rate !== null && remainder > 0;
        }
        updatedAt = now;
        rate = nextRate;
        return average;
      },
    };
  }

  function relativeChange(previous, current) {
    if (previous === null || previous === current) return 0;
    return previous === 0 ? 1 : Math.max(-1, Math.min(1, (current - previous) / previous));
  }

  // TPS 和并发数共用同一时间轴，最近 30 分钟按分钟排列。
  function createTrend(startedAt, wallStartedAt, interval = trendInterval, capacity = trendCapacity) {
    const speed = createAverage(startedAt, interval);
    const count = createAverage(startedAt, interval);
    const points = [];
    return {
      points,
      get deadline() { return speed.deadline; },
      record(now, tps, concurrency) {
        const previousDeadline = speed.deadline;
        const average = speed.record(now, tps);
        const sessions = count.record(now, concurrency);
        if (average === undefined) return;
        const elapsed = (speed.deadline - previousDeadline) / interval;
        const end = wallStartedAt + speed.deadline - interval - startedAt;
        const point = { end, tps: elapsed === 1 ? average : null, concurrency: elapsed === 1 ? sessions : null };
        for (let offset = Math.min(elapsed, capacity) - 1; offset > 0; offset--) {
          points.push({ end: end - offset * interval, tps: null, concurrency: null });
        }
        points.push(point);
        if (points.length > capacity) points.splice(0, points.length - capacity);
        return point;
      },
    };
  }

  window.hudLiveSpeed = { cardSpeed, aggregate, createAverage, createTrend, relativeChange };
})();
