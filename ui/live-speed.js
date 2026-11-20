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

  window.hudLiveSpeed = { cardSpeed, aggregate };
})();
