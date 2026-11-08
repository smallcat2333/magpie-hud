'use strict';

// 同一日期/会话/来源只保留一个费用查询；账本追加不取消仍然有效的逐行定价结果。
(() => {
  const keyOf = scope => JSON.stringify([scope.date, scope.session || null, scope.sourceKey || null, Boolean(scope.historical)]);
  const empty = key => ({ key, status: 'idle', hasData: false, rows: [], warnings: [] });
  function create({ query, changed, now = () => Date.now(), refreshMs = 30_000 }) {
    const entries = new Map();
    let active = null;
    const publish = entry => { if (entry === active) changed(entry.state); };
    const trim = () => {
      for (const [key, entry] of entries) {
        if (entries.size <= 8) break;
        // 在途查询始终保留，切走再切回也不能再发一轮。
        if (entry !== active && !entry.pending) entries.delete(key);
      }
    };
    return {
      update(scope) {
        const key = keyOf(scope);
        if (active?.state.key === key) return;
        if (!entries.has(key)) entries.set(key, { scope: { ...scope }, state: empty(key), pending: null, readAt: null });
        active = entries.get(key);
        entries.delete(key); entries.set(key, active);
        trim();
        publish(active);
      },
      load(force = false) {
        const entry = active;
        if (!entry || !entry.scope.date) return Promise.resolve();
        if (entry.pending) return entry.pending;
        if (!force && entry.readAt !== null && now() - entry.readAt < refreshMs) return Promise.resolve();
        entry.state = { ...entry.state, status: 'loading' };
        // 先保存 Promise，再通知界面；通知期间重入也会复用它。
        entry.pending = Promise.resolve().then(() => query(entry.scope)).then(result => {
          if (result.date !== entry.scope.date) throw new Error('费用日期不匹配，请重试');
          if (result.fetchFailed) throw new Error('费用读取失败，请重试');
          const rows = Array.isArray(result.rows) ? result.rows : [];
          if (entry.state.hasData && entry.state.rows.some(row => row.priced > 0)
              && (!rows.length || rows.every(row => !row.priced && !row.unpriced))) {
            throw new Error('费用尚未匹配，请重试');
          }
          entry.state = { key: entry.state.key, status: 'ready', hasData: true, rows, warnings: result.warnings || [] };
        }).catch(error => {
          entry.state = { ...entry.state, status: 'error', warnings: [String(error)] };
        }).finally(() => {
          entry.pending = null;
          // 冷却从完成时计算，慢查询不会一结束就被新 revision 再次触发。
          entry.readAt = now();
          trim();
          publish(entry);
        });
        publish(entry);
        return entry.pending;
      },
      // 换数据源时丢掉全部缓存；在途响应返回后因 active 已换而不会写回。
      reset() {
        entries.clear();
        active = null;
        changed(empty(''));
      },
    };
  }
  const api = { create };
  if (typeof module !== 'undefined') module.exports = api;
  else window.hudModelCosts = api;
})();
