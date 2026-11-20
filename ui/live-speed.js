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

  const trendCapacity = 30;
  const minuteOf = time => Math.floor(time / trendInterval) * trendInterval;

  // 休眠、断线和打开时的半分钟由完成记录回填。实时值只用于完整观测到的分钟。
  function createTrend(startedAt) {
    let deadline = minuteOf(startedAt) + trendInterval;
    let sampledAt = startedAt;
    let rate = null;
    let count = null;
    let covered = 0;
    let output = 0;
    let busy = 0;
    let hasRate = false;
    const points = new Map();

    function accumulate(elapsed) {
      if (elapsed <= 0 || count === null) return;
      covered += elapsed;
      output += (rate ?? 0) * elapsed;
      busy += count * elapsed;
      hasRate ||= rate !== null;
    }

    return {
      points,
      get deadline() { return deadline; },
      record(now, nextRate, nextCount) {
        const previousDeadline = deadline;
        const clockWentBack = now < sampledAt;
        const gap = clockWentBack || now - sampledAt > trendInterval;
        if (!gap) accumulate(Math.min(now, deadline) - sampledAt);
        if (now >= deadline || clockWentBack) {
          if (!gap && covered === trendInterval) {
            points.set(deadline, {
              tps: hasRate ? output / trendInterval : null,
              concurrency: busy / trendInterval,
            });
          }
          const end = minuteOf(now);
          for (const time of points.keys()) {
            if (time > end || time <= end - trendCapacity * trendInterval) points.delete(time);
          }
          deadline = end + trendInterval;
          covered = output = busy = 0;
          hasRate = false;
          if (!gap) accumulate(now - end);
        } else if (gap) {
          covered = output = busy = 0;
          hasRate = false;
        }
        sampledAt = now;
        if (gap) rate = count = null;
        if (nextRate !== undefined) rate = nextRate;
        if (nextCount !== undefined) count = nextCount;
        return deadline !== previousDeadline;
      },
    };
  }

  /** 输出按生成时段均摊，同一个会话重叠的主请求和子请求只算一次并发。 */
  function recentTrend(requests, live, now, hasLog) {
    const end = minuteOf(now);
    const from = end - trendCapacity * trendInterval;
    const buckets = Array.from({ length: trendCapacity }, () => ({ output: 0, busy: new Map() }));
    for (const { session, start, end: finish, firstToken, output } of requests) {
      if (!Number.isFinite(start) || !Number.isFinite(finish) || finish <= start || finish > now || !session) continue;
      const generationStart = Number.isFinite(firstToken) && firstToken >= start && firstToken < finish ? firstToken : start;
      for (let at = Math.max(from, minuteOf(start)); at < Math.min(finish, end); at += trendInterval) {
        const bucket = buckets[(at - from) / trendInterval];
        const stop = Math.min(finish, at + trendInterval);
        const generated = Math.max(0, stop - Math.max(at, generationStart));
        if (validSpeed(output)) bucket.output += output * generated / (finish - generationStart);
        if (!bucket.busy.has(session)) bucket.busy.set(session, []);
        bucket.busy.get(session).push([Math.max(start, at), stop]);
      }
    }
    return buckets.map(({ output, busy }, index) => {
      const end = from + (index + 1) * trendInterval;
      let occupied = 0;
      for (const intervals of busy.values()) {
        let last = end - trendInterval;
        for (const [start, stop] of intervals.sort((a, b) => a[0] - b[0])) {
          occupied += Math.max(0, stop - Math.max(last, start));
          last = Math.max(last, stop);
        }
      }
      const point = live.get(end);
      const recorded = hasLog || busy.size > 0;
      return {
        end,
        tps: point?.tps ?? (recorded ? output * 1000 / trendInterval : null),
        concurrency: point?.concurrency ?? (recorded ? occupied / trendInterval : null),
      };
    });
  }

  window.hudLiveSpeed = { cardSpeed, aggregate, createAverage, createTrend, recentTrend, relativeChange };
})();
