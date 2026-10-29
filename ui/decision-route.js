'use strict';

// 规则段是已记录的有序决策；尝试段只展示实际发生的请求，不模拟内部分类进度。
(() => {
  const root = document.getElementById('decision-route');
  const heading = document.getElementById('decision-session');
  const status = document.getElementById('decision-status');
  const content = document.getElementById('decision-content');
  const routes = document.getElementById('routes');
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  const ns = 'http://www.w3.org/2000/svg';
  const states = { running: '进行中', waiting: '等待中', success: '已完成', failed: '失败', cancelled: '已取消', unknown: '结果未知' };
  const sources = { classified: '本轮判定', held: '沿用轮初', missing: '轮初判定缺失', revised: '重新判定', unknown: '判定来源未记录' };
  const strategies = { order: '按顺序', rotate: '轮换', usage: '按用量' };
  const waits = { slot: '等待并发空位', rpm: '等待速率额度', retry: '等待路由切换', first_response: '首字状态未确认', unknown: '阶段未记录' };
  const known = new Map(), retained = new Map(), turns = new Map();
  let current = null, latest = new Map(), pending = null, startedAt = 0, timer = 0;
  let hoverScope = null, focusScope = null, restoreTimer = 0;
  let geometry = '', frame = 0, detailAnchor = null, detailTimer = 0, wireTimer = 0;
  const passedEdges = new Map();
  const wireLayer = document.createElementNS(ns, 'svg');
  wireLayer.classList.add('decision-wires'); wireLayer.setAttribute('aria-hidden', 'true');
  const detail = document.createElement('div');
  detail.id = 'decision-detail'; detail.className = 'decision-detail'; detail.hidden = true;
  detail.setAttribute('role', 'tooltip'); document.body.append(detail);
  const node = (tag, className, value) => {
    const element = document.createElement(tag); element.className = className;
    if (value !== undefined) element.textContent = value;
    return element;
  };
  const active = view => view && view.active !== false && ['running', 'waiting'].includes(view.state);
  const ended = view => ['success', 'failed', 'cancelled'].includes(view.state);
  const scopeKey = view => JSON.stringify([view.session, view.subagentId || null]);

  function icon(rule, model) {
    const held = rule?.state === 'held';
    const classifier = model || rule?.classifier;
    const family = classifier && window.hudModelIcon?.(classifier.split('/').at(-1));
    const element = node('span', 'decision-origin');
    element.dataset.origin = held ? 'reused' : classifier ? 'model' : 'unrecorded';
    const label = held ? '沿用轮初判定' : classifier ? `模型：${classifier}` : '判定模型未记录';
    element.setAttribute('role', 'img'); element.setAttribute('aria-label', label);
    if (!held && family) {
      const image = document.createElement('img'); image.src = `icons/${family}.svg`; image.alt = ''; element.append(image);
    } else {
      const svg = document.createElementNS(ns, 'svg'); svg.setAttribute('viewBox', '0 0 24 24');
      const path = document.createElementNS(ns, 'path');
      path.setAttribute('d', held ? 'M12 2.5 8 5.5 12 8.5M8 5.5h4a8 8 0 1 1-8 8M9 13.5l2.5 2.5 5.5-5.5'
        : classifier ? 'M6 7h12v10H6zM9 3v4M15 3v4M9 17v4M15 17v4M2 10h4M18 10h4M2 14h4M18 14h4'
          : 'M14 3H6a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8l-5-5ZM14 3v5h5M9 14h6M9 17h3');
      svg.append(path); element.append(svg);
    }
    return element;
  }

  function closeDetail() {
    if (detailAnchor) detailAnchor.removeAttribute('aria-describedby');
    detailAnchor = null; detail.hidden = true;
  }
  function openDetail(anchor) {
    clearTimeout(detailTimer);
    detailAnchor?.removeAttribute('aria-describedby'); detailAnchor = anchor;
    detail.replaceChildren(node('strong', '', anchor._title));
    for (const line of anchor._lines) detail.append(node('p', '', line));
    anchor.setAttribute('aria-describedby', detail.id); detail.hidden = false;
    const r = anchor.getBoundingClientRect(), box = detail.getBoundingClientRect();
    detail.style.left = `${Math.max(8, Math.min(innerWidth - box.width - 8, r.left))}px`;
    detail.style.top = `${Math.max(8, r.top - box.height - 8)}px`;
  }
  content.addEventListener('pointerover', event => {
    const item = event.target.closest('.decision-step'); if (item) openDetail(item);
  });
  content.addEventListener('pointerout', event => {
    if (content.contains(event.relatedTarget) || detail.contains(event.relatedTarget)) return;
    detailTimer = setTimeout(closeDetail, 140);
  });
  content.addEventListener('focusin', event => { const item = event.target.closest('.decision-step'); if (item) openDetail(item); });
  content.addEventListener('focusout', event => { if (!detail.contains(event.relatedTarget)) closeDetail(); });
  detail.addEventListener('pointerenter', () => clearTimeout(detailTimer));
  detail.addEventListener('pointerleave', closeDetail);
  document.addEventListener('keydown', event => { if (event.key === 'Escape') closeDetail(); });
  window.addEventListener('blur', closeDetail);

  function step(key, label, summary, title, lines, origin, state, kind, strategy) {
    const item = node('button', 'decision-step'); item.type = 'button'; item.dataset.key = key;
    item.dataset.state = state || ''; item.dataset.kind = kind;
    if (kind === 'fold') item.dataset.passed = 'true';
    item._title = title; item._lines = lines;
    item.setAttribute('aria-label', [title, strategy, summary].filter(Boolean).join('，'));
    if (origin) item.append(origin);
    const copy = node('span', 'decision-copy');
    const header = node('span', 'decision-header');
    header.append(node('strong', 'decision-value', label));
    if (strategy) header.append(node('span', 'decision-strategy', strategy));
    copy.append(header, node('span', 'decision-label', summary));
    item.append(copy); return item;
  }
  function decisionDuration(rule) {
    const ms = rule?.durationMs;
    if (!Number.isFinite(ms) || ms < 0 || ['held', 'missing'].includes(rule.state)) return null;
    return { text: ms < 1000 ? `${Math.round(ms)}ms` : `${(ms / 1000).toFixed(2)}s`,
      level: ms < 1000 ? 'fast' : ms < 2000 ? 'moderate' : 'slow' };
  }
  function ruleLines(rule) {
    if (!rule) return ['该组未记录规则判定'];
    return [sources[rule.state] || sources.unknown,
      rule.turn != null && `用户轮次：${rule.turn}`,
      rule.n != null ? rule.n > 0 ? `命中规则：${rule.n}` : '未命中规则' : '规则编号未记录',
      rule.classifier && `判定模型：${rule.classifier}`,
      decisionDuration(rule) && `判定耗时：${rule.durationMs}ms（${rule.cached ? '命中判定缓存' : 'Magpie 判定调用总耗时'}）`,
      !decisionDuration(rule) && !['held', 'missing'].includes(rule.state) && '判定耗时未记录',
      rule.cached && '本次命中判定缓存，不代表重新调用模型',
      rule.intent && `${rule.intentSource === 'previous' ? '沿用上一轮意图' : rule.intentSource === 'after' ? '上一轮意图线索' : '本轮意图'}：${rule.intent}`,
      Number.isFinite(rule.confidence) && `置信度：${(rule.confidence * 100).toFixed(0)}%`,
      rule.effort && `判定强度：${rule.effort}`, rule.target && `规则优先目标：${rule.target}`,
      rule.unready && '规则优先目标当前不可用', rule.instead && `记录的替代目标：${rule.instead}`,
      rule.state === 'held' && '沿用本轮开始时的判定，不表示本次重新调用分类模型'].filter(Boolean);
  }
  function decisionLabel(rule) {
    if (!rule) return '';
    const model = (rule.classifier || '').split('/').at(-1);
    if (rule.state === 'held') return model ? `沿用 ${model}` : '沿用轮初';
    if (rule.state === 'missing') return '轮初判定缺失';
    if (model) return rule.cached ? `${model} · 缓存` : model;
    return '判定模型未记录';
  }
  function buildParts(view) {
    const recorded = Array.isArray(view.stages) ? view.stages : [];
    const attempts = !view.turnOpeningMissing && Array.isArray(view.attempts) ? view.attempts : [];
    const actualChain = attempts.at(-1)?.groupChain || [];
    const stages = actualChain.length ? actualChain.map((group, index) => {
      const evidence = recorded.find(stage => group.id && stage.group?.id === group.id
        || !group.id && group.ref && stage.group?.ref === group.ref);
      return evidence || { key: `actual:${group.id || group.ref || index}`, group, rule: null };
    }) : recorded.filter(stage => stage.confirmed === true);
    const capacity = content.clientWidth < 450 ? 3 : content.clientWidth < 700 ? 4 : 6;
    const parts = [];
    const stageNode = (stage, index) => {
      const group = stage.group || {}, rule = stage.rule;
      const strategy = strategies[group.routing] || group.routing || '';
      const decision = decisionLabel(rule);
      const summary = [rule?.n != null && (rule.n > 0 ? `规则${rule.n}` : '未命中'),
        rule?.turn != null && `第${rule.turn}轮`, rule?.effort].filter(Boolean).join(' · ');
      const lines = [view.turnKey && '按用户输入轮次展示；同轮内部请求不单独切换',
        view.turnOpeningMissing && '本轮开始时的决策未被观察到，仅保留首次可用记录',
        strategy && `候选基础排序：${strategy}；规则命中的目标优先`, ...ruleLines(rule)].filter(Boolean);
      if (stages.length > 2 && index === stages.length - 1 && capacity < 5) {
        lines.push(`中间 ${stages.length - 2} 级：`);
        stages.slice(1, -1).forEach(stage => lines.push(stage.group?.name || '组名未记录', ...ruleLines(stage.rule)));
      }
      if (index === stages.length - 1 && recorded.length) {
        lines.push('请求记录的判定上下文（不代表并行执行）：');
        recorded.forEach(stage => lines.push(`${stage.group?.name || '组名未记录'}${stage.confirmed === false ? '（路径未确认）' : ''}`, ...ruleLines(stage.rule)));
      }
      if (index === stages.length - 1 && view.candidatePlan?.items?.length) {
        lines.push('计划尝试顺序（不是已发生的尝试）：');
        view.candidatePlan.items.forEach(candidate => lines.push(`${candidate.ordinal}. ${candidate.model || '模型未记录'}${candidate.fixed ? ` · 固定 ${candidate.fixed}` : ''}`));
        if (view.candidatePlan.omitted) lines.push(`另 ${view.candidatePlan.omitted} 项未展开`);
      }
      const item = step(`stage:${stage.key || index}`, group.name || '组名未记录', summary, group.name || '路由组', lines, rule ? icon(rule) : null, '', 'group', decision || (strategy && `候选${strategy}`));
      item.dataset.passed = String(actualChain.length > 0 || stage.confirmed === true);
      const duration = decisionDuration(rule);
      item._duration = duration;
      if (duration) item.setAttribute('aria-label', `${item.getAttribute('aria-label')}，判定耗时 ${rule.durationMs} 毫秒`);
      return item;
    };
    if (stages.length) {
      parts.push(stageNode(stages[0], 0));
      if (stages.length > 2 && capacity >= 5) parts.push(step('stage:fold', `另 ${stages.length - 2} 级`, '有序子组', '中间组路径', stages.slice(1, -1).flatMap(stage => [stage.group?.name || '组名未记录', ...ruleLines(stage.rule)]), null, '', 'fold'));
      if (stages.length > 1) parts.push(stageNode(stages.at(-1), stages.length - 1));
    } else parts.push(step('stage:unknown', '路径未记录', '等待路由证据', '路由过程未记录', ['此请求没有可用的组路径记录'], null, '', 'group'));
    const room = Math.max(1, capacity - parts.length);
    const visible = attempts.slice(-Math.min(2, room));
    if (attempts.length > visible.length && parts.length + visible.length < capacity) {
      parts.push(step('attempt:fold', `前 ${attempts.length - visible.length} 次`, '实际尝试', '此前的实际尝试', attempts.slice(0, -visible.length).map((a, i) => `${a.ordinal || i + 1}. ${a.model || '模型未记录'}：${states[a.state] || states.unknown}`), null, '', 'fold'));
    }
    visible.forEach((attempt, index) => {
      const ordinal = attempt.ordinal || attempts.length - visible.length + index + 1;
      const last = index === visible.length - 1;
      const phase = view.phase || 'unknown';
      const activity = last && active(view) && !view.turnContinuation ? (view.waitReason || (['slot', 'rpm'].includes(phase) ? phase : phase === 'upstream' ? 'upstream' : 'unknown')) : '';
      const summary = view.turnContinuation ? '本轮决策模型' : activity ? activity === 'upstream' ? '请求中' : waits[activity] || waits.unknown : states[attempt.state] || states.unknown;
      const chain = (attempt.groupChain || []).map(g => typeof g === 'string' ? g : g.name).filter(Boolean);
      const item = step(`attempt:${ordinal}`, attempt.model || '模型未记录', `尝试${ordinal} · ${summary}`, `实际尝试 ${ordinal}`, [
        `发送模型：${attempt.model || '未记录'}`, attempt.effort && `实际强度：${attempt.effort}`,
        `状态：${summary}`, chain.length && `已确认路径：${chain.join(' → ')}`,
        attempt.pathIncomplete && '后续组路径无法唯一确认',
        ...attempts.slice(0, -visible.length).map((a, i) => `此前尝试 ${a.ordinal || i + 1}：${a.model || '未记录'} · ${states[a.state] || states.unknown}`),
      ].filter(Boolean), icon(null, attempt.model), attempt.state, 'attempt');
      item.dataset.activity = activity; item.dataset.passed = 'true'; parts.push(item);
    });
    if (!attempts.length) parts.push(view.turnOpeningMissing
      ? step('attempt:unobserved', '轮初决策未记录', '本轮内部请求已隐藏', '轮初决策未记录', ['HUD 没有观察到本轮开始时的请求，不用后续内部请求替代轮初模型'], null, '', 'attempt')
      : step('attempt:pending', '等待尝试', waits[view.waitReason] || '尚未观察到模型请求', '实际尝试尚未记录', ['不以规则优先目标替代实际使用模型'], null, '', 'attempt'));
    return parts;
  }

  function drawWires(view) {
    frame = 0;
    clearTimeout(wireTimer);
    let nextFinish = Infinity;
    const now = performance.now();
    const items = [...content.querySelectorAll('.decision-step')], box = content.getBoundingClientRect();
    const paths = items.slice(1).map((item, index) => {
      const previous = items[index], a = previous.getBoundingClientRect(), b = item.getBoundingClientRect();
      const x = a.right - box.left + 3, y = a.top + a.height * 2 / 3 - box.top;
      const end = b.left - box.left - 3, endY = b.top + b.height * 2 / 3 - box.top;
      const edgeKey = JSON.stringify([view.id, previous.dataset.key, item.dataset.key]);
      const passed = previous.dataset.passed === 'true' && item.dataset.passed === 'true';
      if (passed && !passedEdges.has(edgeKey)) {
        passedEdges.set(edgeKey, now);
        while (passedEdges.size > 512) passedEdges.delete(passedEdges.keys().next().value);
      }
      const remaining = passed ? 1000 - (now - passedEdges.get(edgeKey)) : 0;
      const flowing = passed && remaining > 0 && !reduced.matches;
      if (flowing) nextFinish = Math.min(nextFinish, remaining);
      return { key: item.dataset.key, d: `M${x} ${y}C${(x + end) / 2} ${y} ${(x + end) / 2} ${endY} ${end} ${endY}`,
        x: (x + end) / 2, timeY: (a.top + a.height / 3 + b.top + b.height / 3) / 2 - box.top, duration: previous._duration,
        attempt: item.dataset.kind === 'attempt', passed: passed && !flowing, flowing };
    });
    if (Number.isFinite(nextFinish)) wireTimer = setTimeout(render, Math.ceil(nextFinish) + 16);
    const signature = JSON.stringify([view.id, box.width, box.height, paths]);
    if (geometry === signature) return;
    geometry = signature; wireLayer.replaceChildren(); wireLayer.setAttribute('viewBox', `0 0 ${box.width} ${box.height}`);
    for (const item of paths) {
      const base = document.createElementNS(ns, 'path'); base.setAttribute('d', item.d); base.classList.add('decision-wire');
      if (item.attempt) base.classList.add('decision-wire-attempt');
      base.dataset.passed = String(item.passed);
      wireLayer.append(base);
      if (item.duration) {
        const time = document.createElementNS(ns, 'text'); time.classList.add('decision-duration');
        time.dataset.level = item.duration.level; time.textContent = item.duration.text;
        time.setAttribute('x', item.x); time.setAttribute('y', item.timeY); time.setAttribute('text-anchor', 'middle');
        time.setAttribute('dominant-baseline', 'central');
        wireLayer.append(time);
      }
      if (item.flowing && !reduced.matches) {
        const packet = document.createElementNS(ns, 'circle'); packet.classList.add('decision-packet');
        packet.setAttribute('r', '2'); packet.style.offsetPath = `path("${item.d}")`;
        wireLayer.append(packet);
      }
    }
  }
  new ResizeObserver(() => { if (!frame) frame = requestAnimationFrame(render); }).observe(content);
  reduced.addEventListener('change', () => render());

  function reconcile(parts, view) {
    const existing = new Map([...content.querySelectorAll('.decision-step')].map(item => [item.dataset.key, item]));
    for (const [index, part] of parts.entries()) {
      const signature = JSON.stringify([part.innerHTML, part.dataset.state, part.dataset.activity, part.dataset.passed, part._lines, part._duration]);
      let item = existing.get(part.dataset.key);
      if (!item || item._signature !== signature) {
        const hadFocus = item === document.activeElement;
        if (detailAnchor === item) closeDetail();
        if (item) item.replaceWith(part); item = part; item._signature = signature;
        if (hadFocus) item.focus({ preventScroll: true });
      }
      existing.delete(part.dataset.key);
      const siblings = [...content.querySelectorAll('.decision-step')];
      if (siblings[index] !== item) content.insertBefore(item, siblings[index] || (wireLayer.parentNode === content ? wireLayer : null));
    }
    existing.forEach(item => { if (detailAnchor === item) closeDetail(); item.remove(); });
    content.querySelector('.decision-empty')?.remove();
    if (wireLayer.parentNode !== content) content.append(wireLayer);
    drawWires(view);
  }

  function previewScope(card) {
    if (!card || card.closest('.motion-exit')) return null;
    const session = card.closest('.session-group')?.dataset.sessionId;
    return session ? { session, subagent: card.dataset.subagentId || null, label: card.dataset.historyLabel || '未命名会话' } : null;
  }
  function previewDecision(scope) {
    return [...known.values()].filter(item => item.session === scope.session && (item.subagentId || null) === scope.subagent)
      .sort((a, b) => (a.startedMs || 0) - (b.startedMs || 0) || a.id.localeCompare(b.id)).at(-1);
  }
  routes.addEventListener('pointerover', event => {
    const card = event.target.closest('.route-card'); if (!card || card.contains(event.relatedTarget)) return;
    clearTimeout(restoreTimer); hoverScope = previewScope(card); render();
  });
  routes.addEventListener('pointerout', event => {
    const card = event.target.closest('.route-card'); if (!card || card.contains(event.relatedTarget)) return;
    restoreTimer = setTimeout(() => { hoverScope = null; render(); }, 160);
  });
  routes.addEventListener('focusin', event => { focusScope = previewScope(event.target.closest('.route-card')); render(); });
  routes.addEventListener('focusout', event => { focusScope = previewScope(event.relatedTarget?.closest('.route-card')); render(); });

  function render() {
    frame = 0;
    const scope = hoverScope || focusScope, view = scope ? previewDecision(scope) : current;
    root.dataset.preview = String(Boolean(scope)); root.dataset.state = view?.state || 'unknown';
    heading.textContent = view ? [view.label || '未命名会话', view.subagent].filter(Boolean).join(' · ') : scope?.label || '当前范围';
    status.textContent = view ? active(view) ? waits[view.waitReason] || states[view.state] : states[view.state] || states.unknown : scope ? '未记录判定' : '等待判定';
    if (!view) {
      closeDetail(); content.replaceChildren(node('span', 'decision-empty', scope ? '此会话暂无可用的判定记录' : '暂无活跃会话的判定记录'));
      clearTimeout(wireTimer);
      geometry = ''; return;
    }
    reconcile(buildParts(view), view);
  }
  function choose(next) {
    if (!next || current?.id === next.id) return;
    current = next; startedAt = performance.now(); pending = null; render();
  }
  function schedule() {
    clearTimeout(timer); if (!pending) return;
    timer = setTimeout(() => {
      const candidate = latest.get(pending); pending = null; if (candidate) choose(candidate);
    }, Math.max(0, 1000 - (performance.now() - startedAt)));
  }
  function userDecisions(decisions) {
    const result = new Map();
    const incoming = new Set(decisions.filter(item => item?.id).map(item => item.id));
    for (const turn of turns.values()) {
      if (!incoming.has(turn.latest.id) && !ended(turn.latest)) turn.latest = { ...turn.latest, active: false, state: 'unknown' };
    }
    const ordered = decisions.filter(item => item?.id).sort((a, b) =>
      (a.startedMs || 0) - (b.startedMs || 0) || a.id.localeCompare(b.id));
    for (const item of ordered) {
      if (!item.turnKey) { result.set(item.id, item); continue; }
      let turn = turns.get(item.turnKey);
      if (!turn) turn = { opening: item, latest: item };
      // 轮初请求可继续补齐自己的尝试；后续工具调用不能覆盖轮初决策。
      if (turn.opening.id === item.id || (!item.continuation &&
          (turn.opening.continuation || (item.startedMs || 0) < (turn.opening.startedMs || 0)))) turn.opening = item;
      if (turn.latest.id === item.id || (item.startedMs || 0) > (turn.latest.startedMs || 0)
          || ((item.startedMs || 0) === (turn.latest.startedMs || 0) && item.id.localeCompare(turn.latest.id) > 0)) turn.latest = item;
      turns.delete(item.turnKey); turns.set(item.turnKey, turn);
      while (turns.size > 128) turns.delete(turns.keys().next().value);
      const opening = turn.opening, latestRequest = turn.latest;
      result.set(item.turnKey, { ...opening, id: item.turnKey,
        startedMs: latestRequest.startedMs, state: latestRequest.state, active: latestRequest.active,
        phase: opening.id === latestRequest.id ? latestRequest.phase : null,
        waitReason: opening.id === latestRequest.id ? latestRequest.waitReason : null,
        turnContinuation: opening.id !== latestRequest.id,
        turnOpeningMissing: opening.continuation === true });
    }
    return [...result.values()];
  }
  function update(decisions, sessions, allSelected) {
    decisions = userDecisions(decisions);
    const incoming = new Set(decisions.filter(Boolean).map(item => item.id));
    for (const [id, item] of known) if (!incoming.has(id) && !ended(item)) known.set(id, { ...item, active: false, state: 'unknown' });
    for (const item of decisions) {
      if (!item?.id) continue;
      known.delete(item.id); known.set(item.id, item);
      retained.delete(scopeKey(item)); retained.set(scopeKey(item), item.id);
    }
    while (known.size > 128) known.delete(known.keys().next().value);
    while (retained.size > 32) retained.delete(retained.keys().next().value);
    const allowed = new Set(sessions.map(session => session.id));
    const eligible = decisions.filter(item => item?.id && (allSelected || allowed.has(item.session)));
    latest = new Map(eligible.map(item => [item.id, item]));
    const candidates = eligible.filter(item => item.active !== false).sort((a, b) => (a.startedMs || 0) - (b.startedMs || 0) || a.id.localeCompare(b.id));
    const candidate = candidates.at(-1) || [...eligible].sort((a, b) =>
      (a.startedMs || 0) - (b.startedMs || 0) || a.id.localeCompare(b.id)).at(-1);
    if (current && !allSelected && !allowed.has(current.session)) { current = null; pending = null; clearTimeout(timer); }
    if (current) current = known.get(current.id) || { ...current, state: ended(current) ? current.state : 'unknown', active: false };
    if (!current) {
      const fallback = [...retained.values()].reverse().map(id => known.get(id)).find(item => item && (allSelected || allowed.has(item.session)));
      if (candidate || fallback) choose(candidate || fallback);
    }
    if (candidate && candidate.id !== current?.id) {
      pending = candidate.id;
      if (performance.now() - startedAt >= 1000) choose(candidate); else schedule();
    } else { pending = null; clearTimeout(timer); }
    render();
  }
  window.hudDecision = { update };
})();
