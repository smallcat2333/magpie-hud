'use strict';
// 与 UI 无关的聚合函数；比率由累计量计算，费用来自精确匹配的逐请求定价。
(() => {
  const costIndexes = new WeakMap();
  function costIndex(costs) {
    if (!costIndexes.has(costs)) {
      const index = new Map();
      for (const group of costs) for (const id of group.rowIds) index.set(id, group);
      costIndexes.set(costs, index);
    }
    return costIndexes.get(costs);
  }
  function aggregate(rows, costs = []) {
    const ids = new Set(rows.map(r => r.id));
    const result = { calls: rows.length, tokens: 0, input: 0, output: 0, cacheRead: 0,
      generation: 0, speedOutput: 0, samples: 0, priced: 0, cost: null,
      success: 0, failed: 0, cancelled: 0, unknown: 0 };
    for (const row of rows) {
      result.tokens += row.tokens; result.input += row.input; result.output += row.output; result.cacheRead += row.cacheRead;
      result[row.outcome in result ? row.outcome : 'unknown'] += 1;
      if (row.generationMs > 0) { result.generation += row.generationMs; result.speedOutput += row.output; result.samples++; }
    }
    const index = costIndex(costs);
    const candidates = new Set(rows.map(row => index.get(row.id)).filter(Boolean));
    for (const group of candidates) {
      if (group.rowIds.length && group.rowIds.every(id => ids.has(id))) {
        result.cost = (result.cost ?? 0) + group.cost;
        result.priced += group.priced;
      }
    }
    if (!rows.length) result.cost = 0;
    result.speed = result.generation ? result.speedOutput * 1000 / result.generation : null;
    result.cache = result.input + result.cacheRead ? 100 * result.cacheRead / (result.input + result.cacheRead) : null;
    result.average = result.priced ? result.cost / result.priced : null;
    return result;
  }
  function group(rows, key, costs) {
    const bins = new Map();
    rows.forEach(row => { const k = key(row); if (!bins.has(k)) bins.set(k, []); bins.get(k).push(row); });
    return [...bins].map(([key, members]) => ({ key, rows: members, stats: aggregate(members, costs) }));
  }
  function filter(rows, filters, session = '', cycle = null) {
    return rows.filter(r => (!session || r.session === session)
      && Object.entries(filters).every(([key, values]) => !values.size || values.has(r[key]))
      && (!cycle || r.provider === cycle.provider && r.account === cycle.account && r.timestamp >= cycle.start && r.timestamp < cycle.end));
  }
  const api = { aggregate, group, filter };
  if (typeof module !== 'undefined') module.exports = api;
  else window.usageDetailsData = api;
})();
