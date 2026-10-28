'use strict';

// 按需履历浮层独立于反复重建的卡片；外部字符串只写入文本节点。
(() => {
  const routes = document.getElementById('routes');
  const panel = document.createElement('section');
  panel.id = 'route-history';
  panel.className = 'route-history';
  panel.hidden = true;
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-modal', 'false');
  panel.setAttribute('aria-labelledby', 'history-title');
  const node = (tag, name, text) => {
    const element = document.createElement(tag);
    if (name) element.className = name;
    if (text != null) element.textContent = text;
    return element;
  };
  const action = (label, text, fn) => {
    const button = node('button', '', text);
    button.type = 'button';
    button.setAttribute('aria-label', label);
    button.title = label;
    button.addEventListener('click', fn);
    return button;
  };
  const heading = node('header', 'history-heading');
  const identity = node('div', 'history-identity');
  const title = node('strong', '', '请求履历');
  title.id = 'history-title';
  const subtitle = node('span', 'history-subtitle');
  identity.append(title, subtitle);
  const controls = node('div', 'history-controls');
  const refresh = action('刷新履历', '刷新', () => load(true));
  const pin = action('固定当前履历', '固定', () => {
    if (!view) return;
    view.pinned = !view.pinned;
    updatePin();
    if (view.pinned) clearTimeout(hideTimer);
    else scheduleClose();
  });
  pin.setAttribute('aria-pressed', 'false');
  const closeButton = action('关闭请求履历', '×', () => close(true));
  controls.append(refresh, pin, closeButton);
  heading.append(identity, controls);
  const status = node('p', 'history-message');
  status.setAttribute('role', 'status');
  const viewport = node('div', 'history-viewport');
  viewport.tabIndex = 0;
  viewport.setAttribute('aria-label', '请求履历，可滚动查看');
  const table = node('table', 'history-table');
  const caption = node('caption', 'history-caption', '每行一次用户输入；×N 为组内请求数，展开查看每次请求。Token 为最终日志条目口径。');
  const thead = document.createElement('thead');
  const columns = document.createElement('tr');
  ['', '开始时间', '意图', '判定', '模型', '强度', 'Token', 'TPS', '状态'].forEach((text, index) => {
    const cell = node('th', index === 6 || index === 7 ? 'history-number' : '', text);
    cell.scope = 'col';
    if (!text) cell.setAttribute('aria-label', '展开');
    columns.append(cell);
  });
  thead.append(columns);
  const body = document.createElement('tbody');
  table.append(caption, thead, body);
  viewport.append(table);
  const footer = node('footer', 'history-footer');
  const coverage = node('span', 'history-coverage');
  const more = action('展开最近 200 组输入', '展开至 200 组', () => {
    if (!view) return;
    view.limit = 200;
    load(true);
  });
  more.hidden = true;
  footer.append(coverage, more);
  panel.append(heading, status, viewport, footer);
  document.body.append(panel);

  const cache = new Map();
  const fixtures = new Map();
  let view = null;
  let showTimer = 0;
  let pendingOpen = null;
  let hideTimer = 0;
  let refreshTimer = 0;
  let generation = 0;
  let requestSequence = 0;
  let dismissed = '';
  let pointer = null;
  let focusedScope = null;
  let keyboardInput = false;
  const keyOf = scope => JSON.stringify([scope.session, scope.subagent || '']);
  const cardAt = target => target instanceof Element ? target.closest('.route-card[data-history-scope]:not(.motion-exit)') : null;
  const scopeOf = card => {
    if (!card || card.closest('.motion-exit')) return null;
    try { return JSON.parse(card.dataset.historyScope); } catch { return null; }
  };
  const anchorFor = scope => [...routes.querySelectorAll('.route-card[data-history-scope]')]
    .find(card => !card.closest('.motion-exit') && card.dataset.historyScope === keyOf(scope));
  // data-history-scope is the identity tuple; display metadata belongs to the new node.
  const metaOf = card => {
    const tuple = scopeOf(card);
    return tuple ? { session: tuple[0], subagent: tuple[1], label: card.dataset.historyLabel, source: card.dataset.historySource } : null;
  };

  function updatePin() {
    pin.setAttribute('aria-pressed', String(Boolean(view?.pinned)));
    pin.textContent = view?.pinned ? '已固定' : '固定';
    panel.dataset.pinned = String(Boolean(view?.pinned));
  }

  function position() {
    if (!view || panel.hidden) return;
    const anchor = anchorFor(view.scope);
    const rect = anchor?.getBoundingClientRect() || view.rect;
    if (!rect) return;
    view.rect = rect;
    panel.style.maxHeight = `${Math.max(80, innerHeight - 16)}px`;
    const width = panel.offsetWidth;
    const height = panel.offsetHeight;
    const below = rect.bottom + 8;
    const above = rect.top - height - 8;
    const top = below + height <= innerHeight - 8 ? below : above >= 8 ? above : Math.max(8, (innerHeight - height) / 2);
    panel.style.left = `${Math.max(8, Math.min(innerWidth - width - 8, rect.right - width))}px`;
    panel.style.top = `${Math.max(8, Math.min(innerHeight - height - 8, top))}px`;
  }

  function containsInteraction() {
    if (!view) return false;
    const focused = document.activeElement;
    if (keyboardInput && panel.contains(focused)) return true;
    const focusScope = metaOf(cardAt(focused));
    if (keyboardInput && focusScope && keyOf(focusScope) === keyOf(view.scope)) return true;
    if (!pointer) return false;
    const element = document.elementFromPoint(pointer.x, pointer.y);
    const hoveredScope = metaOf(cardAt(element));
    return panel.contains(element) || Boolean(hoveredScope && keyOf(hoveredScope) === keyOf(view.scope));
  }

  function scheduleClose() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (view && !view.pinned && !containsInteraction()) close(false, true);
    }, 200);
  }

  function close(restoreFocus = false, preserveOpening = false) {
    if (!preserveOpening) {
      clearTimeout(showTimer);
      pendingOpen = null;
    }
    clearTimeout(hideTimer);
    clearTimeout(refreshTimer);
    generation += 1;
    if (!view) return;
    const anchor = anchorFor(view.scope);
    dismissed = keyOf(view.scope);
    const needsFocus = restoreFocus && panel.contains(document.activeElement);
    view = null;
    panel.hidden = true;
    routes.querySelectorAll('[aria-controls="route-history"]').forEach(card => card.setAttribute('aria-expanded', 'false'));
    if (needsFocus) anchor?.focus({ preventScroll: true });
  }

  function open(card, keyboard = false) {
    const scope = metaOf(card);
    if (!scope || !scope.session || view?.pinned && keyOf(view.scope) !== keyOf(scope)) return;
    clearTimeout(showTimer);
    pendingOpen = null;
    clearTimeout(hideTimer);
    const same = view && keyOf(view.scope) === keyOf(scope);
    if (!same) {
      generation += 1;
      view = { scope, pinned: false, limit: 50, expanded: new Set(), data: null, rect: card.getBoundingClientRect() };
      viewport.scrollTop = 0;
      viewport.scrollLeft = 0;
      body.replaceChildren();
      table.hidden = true;
      coverage.textContent = '最近 7 天 · 按需读取';
      more.hidden = true;
    } else view.scope = scope;
    dismissed = '';
    panel.hidden = false;
    routes.querySelectorAll('[aria-controls="route-history"]').forEach(element => element.setAttribute('aria-expanded', String(element === card)));
    title.textContent = `${scope.label || '未命名会话'} · 请求履历`;
    title.title = title.textContent;
    subtitle.textContent = window.hudRedact.scrub([scope.source, scope.subagent ? '子 Agent' : '主会话'].filter(Boolean).join(' · '));
    subtitle.title = `${scope.session}${scope.subagent ? ` / ${scope.subagent}` : ''}`;
    updatePin();
    position();
    load(false);
    if (keyboard) viewport.focus({ preventScroll: true });
  }

  function queueOpen(card) {
    const scope = metaOf(card);
    if (!scope || dismissed === keyOf(scope) || view?.pinned) return;
    clearTimeout(hideTimer);
    const key = keyOf(scope);
    if (pendingOpen === key || view && keyOf(view.scope) === key) return;
    clearTimeout(showTimer);
    pendingOpen = key;
    showTimer = setTimeout(() => {
      pendingOpen = null;
      const anchor = anchorFor(scope);
      if (anchor && (anchor.matches(':hover') || keyboardInput && anchor.contains(document.activeElement))) open(anchor);
    }, 400);
  }

  function getData(scope, limit, force) {
    const key = JSON.stringify([scope.session, scope.subagent, limit]);
    const previous = cache.get(key);
    if (previous?.pending) return previous.pending;
    if (!force && previous?.data && performance.now() - previous.at < 1000) return Promise.resolve(previous.data);
    const entry = previous || {};
    const query = window.__TAURI__
      ? window.__TAURI__.core.invoke('query_route_history', { session: scope.session, subagent: scope.subagent || null, limit })
      : fixtures.has(keyOf(scope)) ? Promise.resolve(fixtures.get(keyOf(scope))) : Promise.reject(new Error('预览尚未提供履历数据'));
    entry.pending = Promise.resolve(query).then(data => {
      entry.data = data;
      entry.at = performance.now();
      return data;
    }).finally(() => { entry.pending = null; });
    cache.delete(key);
    cache.set(key, entry);
    while (cache.size > 8) cache.delete(cache.keys().next().value);
    return entry.pending;
  }

  async function load(force) {
    if (!view) return;
    if (document.hidden) {
      clearTimeout(refreshTimer);
      refreshTimer = setTimeout(() => load(false), 1000);
      return;
    }
    const current = view;
    const token = generation;
    const request = ++requestSequence;
    const limit = current.limit;
    clearTimeout(refreshTimer);
    if (!current.data) {
      table.hidden = true;
      status.textContent = '正在读取请求履历…';
      status.hidden = false;
    }
    panel.setAttribute('aria-busy', 'true');
    try {
      const data = await getData(current.scope, limit, force);
      if (view !== current || token !== generation || request !== requestSequence || current.limit !== limit) return;
      current.data = data;
      render(data);
    } catch (error) {
      if (view !== current || token !== generation || request !== requestSequence) return;
      status.hidden = false;
      status.textContent = `履历读取失败：${String(error?.message || error)}`;
      status.dataset.error = 'true';
    } finally {
      if (view === current && token === generation && request === requestSequence) {
        panel.setAttribute('aria-busy', 'false');
        position();
        refreshTimer = setTimeout(() => load(false), 1000);
      }
    }
  }

  const number = value => Number.isFinite(value) ? value.toLocaleString('zh-CN') : '—';
  // 单元格用带单位的紧凑数值，精确值在悬停说明中保留。
  const compact = value => !Number.isFinite(value) ? '—' : value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${(value / 1e3).toFixed(1)}k` : String(value);
  const labels = { success: '完成', failed: '失败', cancelled: '取消', unknown: '未知', running: '进行中', waiting: '等待中', awaiting_usage: '待用量' };
  const intentSourceNote = { previous: '分类失败，沿用上一轮意图', after: '本轮未分类，沿用上一轮意图' };
  // 无分组数据的旧响应（预览 fixture）按单请求组展示，保持面板可用。
  const groupsOf = data => Array.isArray(data.groups) ? data.groups
    : (data.rows || []).map(row => ({ id: row.id, turn: null, count: 1, timestamp: row.timestamp,
      ruleIntent: row.ruleIntent, ruleSure: row.ruleSure, intentSource: row.ruleIntent ? 'measured' : 'none',
      ruleTarget: row.ruleTarget, routeGroups: row.routeGroups, models: [row.served || row.model].filter(Boolean),
      efforts: [row.effort].filter(Boolean), tokens: row.tokens, tokensPerSecond: row.tokensPerSecond,
      outcome: row.outcome, requests: [row] }));
  function render(data) {
    table.hidden = false;
    const scroll = { top: viewport.scrollTop, left: viewport.scrollLeft };
    const fragment = document.createDocumentFragment();
    const cellOf = (tr, value, className = '', detail = '') => {
      const td = node('td', className);
      const text = node('span', 'history-cell', window.hudRedact.scrub(value));
      text.title = window.hudRedact.scrub(detail || String(value));
      td.append(text);
      tr.append(td);
      return td;
    };
    const clockOf = timestamp => {
      const time = new Date(timestamp);
      const valid = Number.isFinite(time.getTime());
      const clock = valid ? time.toLocaleTimeString('zh-CN', { hour12: false }) : '—';
      const display = valid && time.toDateString() !== new Date().toDateString()
        ? `${String(time.getMonth() + 1).padStart(2, '0')}-${String(time.getDate()).padStart(2, '0')} ${clock.slice(0, 5)}` : clock;
      return { display, detail: valid ? time.toLocaleString('zh-CN', { hour12: false }) : '开始时间未记录' };
    };
    const effortOf = (td, effort) => {
      const badge = td.firstElementChild;
      badge.classList.add('effort');
      const level = (effort || '').toLowerCase();
      badge.textContent = level ? level.replace(/^[a-z]/, letter => letter.toUpperCase()) : '—';
      badge.title = effort || '强度未记录';
      if (['low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(level)) badge.dataset.level = level;
    };
    for (const group of groupsOf(data)) {
      const requests = group.requests || [];
      const foldable = (group.count || requests.length) > 1;
      const expanded = foldable && view.expanded.has(group.id);
      const tr = document.createElement('tr');
      tr.dataset.outcome = group.outcome;
      tr.dataset.groupId = group.id;
      if (foldable) tr.dataset.foldable = 'true';
      if (expanded) tr.dataset.expanded = 'true';
      const toggle = node('td', 'history-expand');
      if (foldable) {
        const button = node('button', 'history-expand-button', expanded ? '▾' : '▸');
        button.type = 'button';
        button.setAttribute('aria-expanded', String(expanded));
        button.setAttribute('aria-label', `${expanded ? '收起' : '展开'}该输入的 ${group.count} 次请求`);
        button.addEventListener('click', () => {
          if (!view) return;
          if (view.expanded.has(group.id)) view.expanded.delete(group.id);
          else view.expanded.add(group.id);
          render(view.data);
        });
        toggle.append(button);
      }
      tr.append(toggle);
      const clock = clockOf(group.timestamp);
      cellOf(tr, clock.display, 'history-time', clock.detail);
      const intentText = group.ruleIntent && group.ruleIntent.length > 14 ? `${group.ruleIntent.slice(0, 13)}…` : group.ruleIntent || '';
      const sure = Number.isFinite(group.ruleSure) ? `分类置信度 ${(group.ruleSure * 100).toFixed(0)}%` : '';
      cellOf(tr, intentText || '未记录', 'history-intent', [intentSourceNote[group.intentSource], group.ruleIntent || '实时判定意图未记录', sure].filter(Boolean).join('\n'));
      const groups = (group.routeGroups || []).join(' → ');
      cellOf(tr, groups || group.ruleTarget || '未记录', 'history-decision', [group.ruleTarget && `规则目标 ${group.ruleTarget}`, groups && `实际路径 ${groups}`].filter(Boolean).join('\n') || '规则判定未记录');
      const models = group.models || [];
      cellOf(tr, models[0] || '—', 'history-model', models.length > 1 ? `组内模型\n${models.join('\n')}` : models[0] || '模型未记录');
      const efforts = group.efforts || [];
      effortOf(cellOf(tr, '', 'history-effort'), efforts[0]);
      if (efforts.length > 1) tr.querySelector('.history-effort .effort').title = `组内强度 ${efforts.join('、')}`;
      const counted = requests.filter(row => Number.isFinite(row.tokens)).length;
      cellOf(tr, compact(group.tokens), 'history-number', `组内 Token 合计 ${number(group.tokens)}\n${counted}/${requests.length} 次请求有用量记录`);
      cellOf(tr, Number.isFinite(group.tokensPerSecond) ? group.tokensPerSecond.toFixed(1) : '—', 'history-number', '组内输出 Token 合计 ÷ 生成时长合计，单位 tok/s');
      const outcome = labels[group.outcome] || '未知';
      const outcomeCell = cellOf(tr, outcome, 'history-outcome', `${outcome}\n该输入共 ${group.count || requests.length} 次请求`);
      if (foldable) {
        const badge = node('span', 'history-count', ` ×${group.count}`);
        badge.title = `该输入共 ${group.count} 次请求`;
        outcomeCell.firstElementChild.append(badge);
      }
      fragment.append(tr);
      if (!expanded) continue;
      for (const row of requests) {
        const child = document.createElement('tr');
        child.dataset.outcome = row.outcome;
        child.dataset.requestId = row.id;
        child.dataset.childOf = group.id;
        child.append(node('td', 'history-expand'));
        const childClock = clockOf(row.timestamp);
        cellOf(child, childClock.display, 'history-time', childClock.detail);
        const childIntent = row.ruleIntent && row.ruleIntent.length > 14 ? `${row.ruleIntent.slice(0, 13)}…` : row.ruleIntent || '';
        const childSure = Number.isFinite(row.ruleSure) ? `分类置信度 ${(row.ruleSure * 100).toFixed(0)}%` : '';
        cellOf(child, childIntent || '—', 'history-intent', [intentSourceNote[row.ruleFallback === 'previous' ? 'previous' : ''], row.ruleIntent, childSure].filter(Boolean).join('\n') || '该请求未触发分类');
        const childGroups = (row.routeGroups || []).join(' → ');
        cellOf(child, childGroups || row.ruleTarget || '—', 'history-decision', [row.ruleTarget && `规则目标 ${row.ruleTarget}`, childGroups && `实际路径 ${childGroups}`].filter(Boolean).join('\n') || '规则判定未记录');
        const model = row.served || row.model || '—';
        cellOf(child, model, 'history-model', [`发送模型 ${row.model || '未记录'}`, `响应报告模型 ${row.served || '未记录'}`, row.account && `账号 ${window.hudRedact.text(row.account)}`, row.provider && `供应商 ${row.provider}`].filter(Boolean).join('\n'));
        effortOf(cellOf(child, '', 'history-effort'), row.effort);
        cellOf(child, compact(row.tokens), 'history-number', `最终日志条目 Token ${number(row.tokens)}\n输入 ${number(row.input)} · 输出 ${number(row.output)} · 缓存读取 ${number(row.cacheRead)}`);
        cellOf(child, Number.isFinite(row.tokensPerSecond) ? row.tokensPerSecond.toFixed(1) : '—', 'history-number', '输出 Token ÷（总耗时 − 首字耗时），单位 tok/s');
        const childOutcome = labels[row.outcome] || '未知';
        cellOf(child, `${childOutcome}${row.tries > 1 ? ` · ${row.tries}次` : ''}`, 'history-outcome', [childOutcome, row.status != null && `HTTP ${row.status}`, row.tries > 1 && `共 ${row.tries} 次尝试`, `原始会话 ${row.session}`, row.subagent && `子 Agent ${row.subagent}`].filter(Boolean).join('\n'));
        fragment.append(child);
      }
    }
    body.replaceChildren(fragment);
    viewport.scrollTop = scroll.top;
    viewport.scrollLeft = scroll.left;
    const groups = groupsOf(data);
    const warnings = Array.isArray(data.warnings) ? data.warnings.filter(Boolean) : [];
    table.hidden = !groups.length;
    status.dataset.error = 'false';
    status.textContent = !groups.length ? '扫描范围内暂无请求记录' : warnings.join(' · ');
    status.hidden = !status.textContent;
    const range = data.startDate && data.endDate ? `${data.startDate} — ${data.endDate}` : '最近 7 天';
    const requests = groups.reduce((sum, group) => sum + (group.requests || []).length, 0);
    coverage.textContent = `${range} · ${groups.length} 组 ${requests} 次请求${data.byteLimited ? ' · 仅扫描末尾 4 MiB' : ''}${data.rowLimited ? ' · 还有更早记录' : ''}`;
    coverage.title = `一行一组用户输入，×N 为组内请求数。Token 为最终日志条目；重试中间计费未合计。扫描 ${number(data.bytesScanned)} 字节。轮次仅在近期 trace 保留时可用，无记录的请求各自成组。`;
    more.hidden = !data.rowLimited || view.limit >= 200;
  }

  routes.addEventListener('pointerover', event => {
    const card = cardAt(event.target);
    if (!card || card.contains(event.relatedTarget)) return;
    const scope = metaOf(card);
    if (scope && dismissed !== keyOf(scope)) dismissed = '';
    queueOpen(card);
  });
  routes.addEventListener('pointerout', event => {
    const card = cardAt(event.target);
    if (!card || !card.isConnected || card.contains(event.relatedTarget)) return;
    clearTimeout(showTimer);
    pendingOpen = null;
    dismissed = '';
    if (!panel.contains(event.relatedTarget)) scheduleClose();
  });
  routes.addEventListener('focusin', event => queueOpen(cardAt(event.target)));
  routes.addEventListener('focusout', event => {
    if (!panel.contains(event.relatedTarget)) {
      if (event.target.isConnected) dismissed = '';
      scheduleClose();
    }
  });
  routes.addEventListener('keydown', event => {
    const card = cardAt(event.target);
    if (card && event.key === 'ArrowDown' && event.altKey) {
      event.preventDefault();
      dismissed = '';
      open(card, true);
    }
  });
  panel.addEventListener('pointerenter', () => { clearTimeout(hideTimer); clearTimeout(showTimer); });
  panel.addEventListener('pointerleave', scheduleClose);
  panel.addEventListener('focusin', () => clearTimeout(hideTimer));
  panel.addEventListener('focusout', scheduleClose);
  document.addEventListener('pointermove', event => { pointer = { x: event.clientX, y: event.clientY }; }, { passive: true });
  document.addEventListener('pointerdown', event => {
    keyboardInput = false;
    if (!view || panel.contains(event.target)) return;
    const card = cardAt(event.target);
    if (!card || keyOf(metaOf(card)) !== keyOf(view.scope)) close(false);
  });
  document.addEventListener('keydown', event => {
    keyboardInput = true;
    if (event.key === 'Escape' && view) { event.preventDefault(); close(true); }
  });
  window.addEventListener('resize', position);
  document.addEventListener('scroll', event => { if (view && !panel.contains(event.target)) position(); }, true);
  window.addEventListener('blur', () => { if (!view?.pinned) close(false); });
  document.addEventListener('visibilitychange', () => { if (!document.hidden && view) load(false); });
  window.addEventListener('hud-preview-history', event => {
    if (window.__TAURI__) return;
    const { session, subagent = '', data } = event.detail;
    fixtures.set(keyOf({ session, subagent }), data);
    cache.clear();
    if (view) load(true);
  });

  window.hudHistory = {
    attach(card, scope) {
      card.dataset.historyScope = keyOf(scope);
      card.dataset.historyLabel = scope.label || '未命名会话';
      card.dataset.historySource = scope.source || '';
      card.removeAttribute('title');
      // 卡内原生提示会遮住共享面板，完整信息由履历单元格提供。
      card.querySelectorAll('[title]').forEach(element => element.removeAttribute('title'));
      card.setAttribute('aria-haspopup', 'dialog');
      card.setAttribute('aria-controls', panel.id);
      card.setAttribute('aria-expanded', String(Boolean(view && keyOf(view.scope) === keyOf(scope))));
      card.setAttribute('aria-description', '悬停查看请求履历，Alt + 向下键进入履历表');
    },
    beforeRender() {
      focusedScope = metaOf(cardAt(document.activeElement));
    },
    redraw() {
      if (view?.data) {
        subtitle.textContent = window.hudRedact.scrub([view.scope.source, view.scope.subagent ? '子 Agent' : '主会话'].filter(Boolean).join(' · '));
        render(view.data);
      }
    },
    reconcile() {
      if (focusedScope) {
        anchorFor(focusedScope)?.focus({ preventScroll: true });
        focusedScope = null;
      }
      if (!view) {
        const target = keyboardInput ? cardAt(document.activeElement) : pointer ? cardAt(document.elementFromPoint(pointer.x, pointer.y)) : null;
        if (target) queueOpen(target);
        return;
      }
      const anchor = anchorFor(view.scope);
      if (!anchor && !view.pinned) { close(false); return; }
      if (anchor) {
        view.scope = metaOf(anchor);
        title.textContent = `${view.scope.label} · 请求履历`;
        title.title = title.textContent;
        subtitle.textContent = window.hudRedact.scrub([view.scope.source, view.scope.subagent ? '子 Agent' : '主会话'].filter(Boolean).join(' · '));
      }
      position();
      if (containsInteraction()) clearTimeout(hideTimer);
    },
  };
})();
