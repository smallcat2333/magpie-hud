'use strict';

// 仅消费 Rust 结构化快照。账号与日志一律写入 textContent，不把外部文本当 HTML。
(() => {
  const byId = id => document.getElementById(id);
  const hud = byId('hud');
  const selector = byId('session-select');
  const agentSelector = byId('agent-select');
  const agentOptions = byId('agent-options');
  let selectedAgent = '*';
  let agentOptionsKey = '';
  const routeList = byId('routes');
  const lineLayer = byId('route-lines');
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  const namespace = 'http://www.w3.org/2000/svg';
  let snapshot = null;
  let selectedCard = null;
  let renderedIds = new Set();
  let frame = 0;
  const branchViews = new Map();
  const sourceViews = new Map();
  const sourceBranches = new Map();
  const quotaViews = new Map();
  const quotaTooltip = byId('quota-tooltip');
  let quotaTooltipView = null;
  let quotaTooltipShowTimer = 0;
  let quotaTooltipHideTimer = 0;
  let quotaTooltipKey = '';
  const subagentNumbers = new Map();
  const childPresence = new Map();
  const childSessions = new Map();
  let childPresenceDate = '';
  const childGraceMs = 60_000;
  let childGraceTimer = 0;
  const sessionOrder = new Map();
  const sessionGraceMs = 10 * 60_000;
  let sessionOrderDate = '';
  let sessionOrderTimer = 0;
  let sessionOrderRank = 0;
  let sessionFrontRank = 0;
  let quotaRenderKey = '';
  let quotaOrderTimer = 0;

  /** 文本节点保留完整 title，显示受 CSS 单行省略约束。 */
  function text(id, value) {
    const element = byId(id);
    element.textContent = value;
    element.title = value;
  }

  const compactFormat = new Intl.NumberFormat('en', {
    notation: 'compact', minimumSignificantDigits: 3, maximumSignificantDigits: 3, useGrouping: false,
  });

  /** 数量缩写保留三位有效数字，千以下保持整数；精确值留在悬停说明。 */
  function compact(value) {
    const number = Math.round(Number(value));
    return number < 1000 ? String(number) : compactFormat.format(number).replace('K', 'k');
  }

  /** 缩略账号本地部分，完整账号留在可访问名称和提示中。 */
  function shortAccount(account) {
    const at = account.lastIndexOf('@');
    if (at < 0) return account.length > 27 ? `${account.slice(0, 17)}…${account.slice(-7)}` : account;
    const local = account.slice(0, at);
    return `${local.length > 10 ? `${local.slice(0, 5)}…${local.slice(-3)}` : local}${account.slice(at)}`;
  }

  /** 按实际模型族选择随应用打包的图标，未知模型显示中性符号。 */
  function modelIcon(model) {
    const name = model.toLowerCase();
    const families = [
      ['feishu', /feishu|lark|飞书/],
      ['xiaomimimo', /mimo|xiaomi/],
      ['siliconcloud-color', /silicon/],
      ['modelscope-color', /modelscope/],
      ['qwen-color', /qwen|qwq|qvq/],
      ['kimi-color', /kimi|moonshot/],
      ['minimax-color', /minimax/],
      ['mistral-color', /mistral|mixtral|codestral|pixtral/],
      ['stepfun-color', /stepfun|step-/],
      ['groq', /groq/],
      ['ollama', /ollama/],
      ['openrouter-color', /openrouter/],
      ['copilot-color', /copilot/],
      ['vertexai-color', /vertex/],
      ['bedrock-color', /bedrock/],
      ['azure-color', /azure/],
      ['baidu-color', /baidu|qianfan|文心|ernie/],
      ['tencentcloud-color', /tencent|tcloud/],
      ['hunyuan-color', /hunyuan|混元/],
      ['huaweicloud-color', /huawei|盘古/],
      ['volcengine-color', /volcengine|ark|火山/],
      ['doubao-color', /doubao|豆包/],
      ['zai', /\bzai\b|glm|zhipu|智谱/],
      ['together-color', /together/],
      ['fireworks-color', /fireworks/],
      ['nvidia-color', /nvidia|nemotron/],
      ['cline', /cline/],
      ['opencode', /opencode/],
      ['workersai-color', /workers/],
      ['cloudflare-color', /cloudflare/],
      ['nousresearch', /nous/],
      ['antigravity-color', /antigravity/],
      ['geminicli-color', /gemini-?cli/],
      ['xai', /\bxai\b/],
      ['ai302-color', /\b302\b/],
      ['aihubmix-color', /aihubmix/],
      ['commandcode', /commandcode/],
      ['kilocode', /kilo/],
      ['cherrystudio-color', /cherry/],
      ['fishaudio', /fish|鱼/],
      ['bailian-color', /bailian|百炼/],
      ['codex-color', /codex/],
      ['openai', /gpt|o[134](?:-|$)/],
      ['claude-color', /claude|sonnet|opus|haiku|fable/],
      ['grok', /grok/],
      ['gemini-color', /gemini/],
      ['deepseek-color', /deepseek/],
    ];
    const match = families.find(([, pattern]) => pattern.test(name));
    return match ? match[0] : null;
  }

  /** 展示统一为完成、运行、异常三类；异常类别按取消、失败、未知区分。 */
  function statusInfo(card) {
    switch (card.state) {
      case 'intermission': return ['', '间歇', 'intermission'];
      case 'delegating': return ['', '子任务', 'delegating'];
      case 'running': return ['', '运行', 'running'];
      case 'waiting': return ['', '运行', 'waiting'];
      case 'success':
      case 'recovered': return ['√', '完成', 'success'];
      case 'cancelled': return ['x', '取消', 'error'];
      case 'failed':
      default: return ['x', '错误', 'error'];
    }
  }

  /** 时间只更新文本，不触发卡片重建或会话重排。 */
  function updateCardTimes() {
    const now = Date.now();
    routeList.querySelectorAll('.card-time').forEach(element => {
      const updated = Date.parse(element.dataset.updatedAt);
      const valid = Number.isFinite(updated);
      element.querySelector('time').textContent = valid
        ? new Date(updated).toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', hour12: false }) : '—';
      const seconds = valid ? Math.max(0, Math.floor((now - updated) / 1000)) : 0;
      const active = element.closest('.route-card').matches('.running,.waiting');
      element.querySelector('.card-age').textContent = active ? '--' : !valid ? '—' : seconds >= 3600
        ? `${Math.floor(seconds / 3600)}h${Math.floor(seconds % 3600 / 60)}m`
        : `${Math.floor(seconds / 60)}m${String(seconds % 60).padStart(2, '0')}s`;
    });
  }

  /** 创建一块两行路由卡；只有本地固定路径可用于图片 src。 */
  function createCard(card, session) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `route-card ${card.state}${selectedCard === card.id ? ' selected' : ''}`;
    button.dataset.cardId = card.id;
    button.dataset.current = String(card.current);
    button.setAttribute('role', 'listitem');
    const [symbol, state, status] = statusInfo(card);
    button.dataset.status = status;
    const tooltip = `${card.account}\n${session.label}: ${session.id || '日志未提供会话 ID'}\n${card.model} · ${card.effort}\n${card.time} · ${card.detail}${card.error ? `\n${card.error}` : ''}`;
    button.title = tooltip;
    button.setAttribute('aria-label', tooltip.replaceAll('\n', '，'));
    const icon = document.createElement('span');
    icon.className = 'model-icon';
    const selecting = ['running', 'waiting'].includes(card.state) && card.account === '正在选择账号';
    const family = modelIcon(card.model) || (/feishu|lark|飞书/i.test(card.provider || '') ? 'feishu' : null);
    if (family) {
      const image = document.createElement('img');
      image.src = `icons/${family}.svg`;
      image.alt = family.replace('-color', '');
      icon.append(image);
    } else if (selecting || ['running', 'waiting'].includes(card.state) && (!card.model || /^(正在选择账号|正在选择模型|正在选路|模型待定)$/.test(card.model))) {
      icon.title = '正在选路';
      const routing = document.createElementNS(namespace, 'svg');
      routing.setAttribute('class', 'routing-icon');
      routing.setAttribute('viewBox', '0 0 24 24');
      routing.setAttribute('aria-hidden', 'true');
      const path = document.createElementNS(namespace, 'path');
      path.setAttribute('d', 'M5 12h4c4 0 2-7 6-7h3M9 12h9M9 12c4 0 2 7 6 7h3');
      routing.append(path);
      for (const [cx, cy] of [[4, 12], [19, 5], [19, 12], [19, 19]]) {
        const node = document.createElementNS(namespace, 'circle');
        node.setAttribute('cx', cx);
        node.setAttribute('cy', cy);
        node.setAttribute('r', '1.5');
        routing.append(node);
      }
      icon.append(routing);
    } else {
      const fallback = document.createElement('span');
      fallback.className = 'fallback';
      fallback.textContent = '◇';
      fallback.setAttribute('aria-hidden', 'true');
      icon.append(fallback);
    }
    const copy = document.createElement('span');
    copy.className = 'card-copy';
    const top = document.createElement('span');
    top.className = 'card-top';
    const account = document.createElement('span');
    account.className = 'session-title';
    account.textContent = session.label;
    const tag = document.createElement('span');
    tag.className = 'state-tag';
    tag.dataset.status = status;
    tag.setAttribute('aria-label', state);
    tag.title = [state, card.detail].filter(Boolean).join('\n');
    if (['running', 'waiting'].includes(status)) {
      const spinner = document.createElementNS(namespace, 'svg');
      spinner.setAttribute('class', 'request-spinner');
      spinner.setAttribute('viewBox', '0 0 20 20');
      spinner.setAttribute('aria-hidden', 'true');
      const circle = document.createElementNS(namespace, 'circle');
      circle.setAttribute('cx', '10');
      circle.setAttribute('cy', '10');
      circle.setAttribute('r', '7');
      spinner.style.animationDelay = `${-(performance.now() % 1400)}ms`;
      spinner.append(circle);
      tag.append(spinner);
    } else {
      const mark = document.createElement('span');
      mark.className = 'state-symbol';
      mark.textContent = symbol;
      tag.append(mark);
    }
    const stateText = document.createElement('span');
    stateText.className = 'state-text';
    stateText.textContent = state;
    tag.append(stateText);
    top.append(account);
    const bottom = document.createElement('span');
    bottom.className = 'card-bottom';
    const label = document.createElement('span');
    label.className = 'card-account';
    label.textContent = /^(正在选择账号|账号未提供)$/.test(card.account) ? '待定' : shortAccount(card.account);
    label.title = card.account;
    const separator = document.createElement('span');
    separator.className = 'divider';
    separator.textContent = '·';
    const model = document.createElement('span');
    model.className = 'card-model';
    const groups = Array.isArray(card.routeGroups) ? card.routeGroups.filter(group => typeof group === 'string' && group) : [];
    const chain = document.createElement('span');
    chain.className = 'card-route-groups';
    groups.forEach(group => {
      const name = document.createElement('strong');
      name.textContent = group;
      chain.append(name, document.createTextNode(' → '));
    });
    const modelName = document.createElement('span');
    modelName.className = 'card-model-name';
    modelName.textContent = /^(正在选择账号|正在选择模型|模型待定)$/.test(card.model) ? '正在选路' : card.model;
    model.append(chain, modelName);
    model.title = [...groups, card.model].join(' → ');
    if (groups.length) {
      button.title += `\n路由 ${model.title}`;
      button.setAttribute('aria-label', button.title.replaceAll('\n', '，'));
    }
    const effort = document.createElement('span');
    effort.className = 'effort';
    effort.textContent = /^(强度待确定|强度未提供|待定)$/.test(card.effort) ? '待定'
      : card.effort.replace(/^[a-z]/, letter => letter.toUpperCase());
    const intensity = card.effort.toLowerCase();
    if (['low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(intensity)) effort.dataset.level = intensity;
    bottom.append(model, separator, label);
    copy.append(top, tag, bottom, effort);
    const metrics = document.createElement('span');
    metrics.className = 'card-metrics';
    const tps = card.tokensPerSecond;
    const hasSpeed = Number.isFinite(tps) && tps >= 0;
    if (hasSpeed) metrics.dataset.speed = tps < 30 ? 'red' : tps < 60 ? 'yellow' : tps < 120 ? 'blue' : tps < 200 ? 'green' : 'purple';
    const tokens = document.createElement('span');
    tokens.className = 'card-tokens';
    const previous = Boolean(card.metricsArePrevious);
    tokens.textContent = card.tokens == null ? '-- TOK' : `${compact(card.tokens)} TOK`;
    const speed = document.createElement('span');
    speed.className = 'card-tps';
    speed.textContent = hasSpeed ? `${tps.toFixed(1)} TPS` : '-- TPS';
    metrics.title = [card.tokens == null ? '' : `${Number(card.tokens).toLocaleString('zh-CN')} Token`, previous
      ? '进行中请求尚无精确用量；这里显示该会话上一轮已完成请求。Token = 输入 + 输出 + 缓存读取；TPS = 输出 / (总耗时 − 首字耗时)。'
      : '本次请求 Token = 输入 + 输出 + 缓存读取；TPS = 输出 / (总耗时 − 首字耗时)。暂无上轮记录时，进行中等待最终用量。'].filter(Boolean).join('\n');
    metrics.append(tokens, speed);
    const clock = document.createElement('span');
    clock.className = 'card-time';
    clock.dataset.updatedAt = card.updatedAt || '';
    clock.setAttribute('aria-label', '更新时间与距今时长');
    const updated = document.createElement('time');
    updated.dateTime = card.updatedAt || '';
    const age = document.createElement('span');
    age.className = 'card-age';
    clock.append(updated, age);
    clock.title = card.updatedAt ? `最近请求活动 ${new Date(card.updatedAt).toLocaleString('zh-CN', { hour12: false })}` : '更新时间未提供';
    const headingAccount = label.cloneNode(true);
    headingAccount.classList.add('card-heading-account');
    headingAccount.textContent = /^(正在选择账号|账号未提供)$/.test(card.account) ? '待定' : card.account;
    const headingDivider = document.createElement('span');
    headingDivider.className = 'card-heading-divider';
    headingDivider.textContent = '|';
    headingDivider.setAttribute('aria-hidden', 'true');
    top.append(headingDivider, headingAccount);
    copy.append(clock);
    button.append(icon, copy, metrics);
    button.addEventListener('click', () => {
      selectedCard = card.id;
      routeList.querySelectorAll('.route-card,.subagent-row').forEach(element => element.classList.toggle('selected', element === button));
      renderNotes([card.note], [session]);
    });
    return button;
  }

  /** 只挂载有活跃模型请求的子 Agent，编号在所属会话内保持稳定。 */
  function activeSubagents(session) {
    return (session.subagents || []).filter(card => ['running', 'waiting'].includes(card.state));
  }

  function sessionIsActive(session) {
    return session.routes.some(card => ['running', 'waiting'].includes(card.state)) || activeSubagents(session).length > 0;
  }

  /** 父会话独立保序十分钟；只有真实请求活动才重置，子项的一分钟保留不续期。 */
  function updateSessionOrder(next) {
    const now = performance.now();
    const initial = sessionOrderDate !== next.date;
    if (initial) {
      sessionOrder.clear();
      sessionOrderRank = 0;
      sessionFrontRank = 0;
      sessionOrderDate = next.date;
    }
    const newcomers = initial ? [] : next.sessions.filter(session => !sessionOrder.has(session.id));
    sessionFrontRank -= newcomers.length;
    const frontRanks = new Map(newcomers.map((session, index) => [session.id, sessionFrontRank + index]));
    const ids = new Set(next.sessions.map(session => session.id));
    for (const id of sessionOrder.keys()) if (!ids.has(id)) sessionOrder.delete(id);
    for (const session of next.sessions) {
      const active = sessionIsActive(session);
      let entry = sessionOrder.get(session.id);
      if (!entry) {
        entry = { rank: frontRanks.has(session.id) ? frontRanks.get(session.id) : sessionOrderRank++,
          idleAt: active ? null : initial ? -Infinity : now };
        sessionOrder.set(session.id, entry);
      } else if (active) {
        if (entry.idleAt !== null && now - entry.idleAt >= sessionGraceMs) entry.rank = sessionOrderRank++;
        entry.idleAt = null;
      } else if (entry.idleAt === null) entry.idleAt = now;
    }
  }

  function orderedSessions(sessions) {
    clearTimeout(sessionOrderTimer);
    const now = performance.now();
    let remaining = Infinity;
    const protectedSessions = [];
    const recent = [];
    for (const session of sessions) {
      const entry = sessionOrder.get(session.id);
      const delay = entry?.idleAt === null ? Infinity : (entry?.idleAt ?? -Infinity) + sessionGraceMs - now;
      if (sessionIsActive(session) || delay > 0) {
        protectedSessions.push(session);
        if (Number.isFinite(delay)) remaining = Math.min(remaining, delay);
      } else recent.push(session);
    }
    protectedSessions.sort((a, b) => (sessionOrder.get(a.id)?.rank ?? Infinity) - (sessionOrder.get(b.id)?.rank ?? Infinity));
    if (Number.isFinite(remaining)) sessionOrderTimer = setTimeout(() => render(false), remaining + 1);
    return { protectedSessions, recent };
  }

  /** 活跃消失时开始一分钟保留，重复快照不续期，新请求沿用同一子项位置。 */
  function updateChildPresence(next) {
    if (childPresenceDate !== next.date) {
      childPresence.clear();
      childSessions.clear();
      childPresenceDate = next.date;
    }
    const sessions = new Set(next.sessions.map(session => session.id));
    const now = performance.now();
    for (const [id, entries] of childPresence) {
      if (!sessions.has(id)) {
        for (const entry of entries.values()) if (entry.goneAt === null) entry.goneAt = now;
      }
    }
    for (const session of next.sessions) {
      childSessions.set(session.id, session);
      const entries = childPresence.get(session.id) || new Map();
      const active = activeSubagents(session);
      const ids = new Set(active.map(card => card.subagentId || card.id));
      for (const [id, entry] of entries) {
        if (!ids.has(id) && entry.goneAt === null) entry.goneAt = now;
      }
      for (const card of active) entries.set(card.subagentId || card.id, { card, goneAt: null });
      if (entries.size) childPresence.set(session.id, entries);
    }
    expireChildPresence();
  }

  function expireChildPresence() {
    clearTimeout(childGraceTimer);
    const now = performance.now();
    let remaining = Infinity;
    for (const [session, entries] of childPresence) {
      for (const [id, entry] of entries) {
        if (entry.goneAt === null) continue;
        const delay = entry.goneAt + childGraceMs - now;
        if (delay <= 0) entries.delete(id);
        else remaining = Math.min(remaining, delay);
      }
      if (!entries.size) childPresence.delete(session);
    }
    for (const id of childSessions.keys()) {
      if (!childPresence.has(id)) childSessions.delete(id);
    }
    if (Number.isFinite(remaining)) {
      childGraceTimer = setTimeout(() => {
        expireChildPresence();
        updateSelector(sessionsForView());
        render(false);
      }, remaining + 1);
    }
  }

  function retainedSubagents(session) {
    return [...(childPresence.get(session.id)?.values() || [])].filter(entry => entry.goneAt !== null).map(entry => {
      const previous = entry.card;
      const done = (session.completedSubagents || []).find(card => card.subagentId === previous.subagentId
        && card.routeId != null && card.routeId === previous.routeId);
      if (done) return { ...done, id: previous.id, grace: true };
      const detail = '模型请求暂时结束，等待下一次请求或最终用量；空闲满 1 分钟后收起';
      return { ...previous, state: 'intermission', current: false, grace: true, error: null, detail,
        note: { ...previous.note, kind: '间歇', text: detail } };
    });
  }

  /** 总览保留短暂间歇的子项；会话详情补入每个子 Agent 的最近完成记录。 */
  function subagentsForView(session, includeCompleted = false) {
    const active = activeSubagents(session);
    const activeIds = new Set(active.map(card => card.subagentId || card.id));
    const retained = retainedSubagents(session).filter(card => !activeIds.has(card.subagentId || card.id));
    if (!includeCompleted) {
      const ranks = new Map([...(childPresence.get(session.id)?.keys() || [])].map((id, index) => [id, index]));
      return [...active, ...retained].sort((a, b) => (ranks.get(a.subagentId || a.id) ?? Infinity) - (ranks.get(b.subagentId || b.id) ?? Infinity));
    }
    const retainedIds = new Set(retained.map(card => card.subagentId || card.id));
    const completed = (session.completedSubagents || []).filter(card => !activeIds.has(card.subagentId || card.id) && !retainedIds.has(card.subagentId || card.id));
    return [...active, ...retained, ...completed];
  }

  /** 没有完成日志的临时会话也保留子项锚点，但不保留已失活的根请求。 */
  function sessionsForView() {
    const sessions = [...snapshot.sessions];
    const ids = new Set(sessions.map(session => session.id));
    for (const [id, session] of childSessions) {
      if (!ids.has(id) && childPresence.has(id)) sessions.push({ ...session, routes: [], subagents: [] });
    }
    return sessions;
  }

  function hasSessionContent(session) {
    return session.routes.length || subagentsForView(session).length || (session.completedSubagents || []).length;
  }

  function subagentName(card, session) {
    const id = card.subagentId || card.id;
    if (!subagentNumbers.has(session.id)) subagentNumbers.set(session.id, new Map());
    const numbers = subagentNumbers.get(session.id);
    if (!numbers.has(id)) numbers.set(id, numbers.size + 1);
    const label = card.subagentLabel || '';
    return label && label !== `子 Agent ${id.slice(0, 8)}` ? label : `子 Agent ${numbers.get(id)}`;
  }

  /** 共用模型与状态渲染，子项独立按内容排成紧凑的一行。 */
  function createSubagent(card, session) {
    const button = createCard(card, session);
    button.classList.add('subagent-row');
    button.dataset.subagentId = card.subagentId || card.id;
    button.dataset.grace = String(Boolean(card.grace));
    const label = subagentName(card, session);
    button.querySelector('.session-title').textContent = label;
    button.title = `${label}\n子 Agent ID: ${card.subagentId || card.id}\n${button.title}`;
    button.setAttribute('aria-label', button.title.replaceAll('\n', '，'));
    const metrics = button.querySelector('.card-metrics');
    metrics.title = metrics.title.replaceAll('该会话', '该子 Agent');
    const copy = button.querySelector('.card-copy');
    const name = copy.querySelector('.session-title');
    name.title = label;
    const state = copy.querySelector('.state-tag');
    const model = copy.querySelector('.card-model');
    const account = copy.querySelector('.card-account');
    const effort = copy.querySelector('.effort');
    const created = document.createElement('time');
    created.className = 'subagent-created';
    const start = /^(\d{2}):(\d{2}):\d{2}$/.exec(card.time || '');
    created.textContent = start ? `${start[1]}:${start[2]}` : '—';
    if (start) created.dateTime = card.time;
    created.title = start ? `请求创建时间 ${card.time}` : '请求创建时间未提供';
    created.setAttribute('aria-label', created.title);
    button.replaceChildren(button.querySelector('.model-icon'), name, model, created, state, effort, metrics);
    button.title += `\n账号: ${account.title}`;
    button.setAttribute('aria-label', button.title.replaceAll('\n', '，'));
    return button;
  }

  /** 尚无主请求时仅提供会话锚点，不用子 Agent 的结果冒充主请求。 */
  function parentAnchor(session) {
    const note = session.notes[0];
    return {
      id: `parent-${session.id}`, sessionId: session.id, sessionLabel: session.label,
      account: '账号未提供', model: activeSubagents(session).length ? '子 Agent 请求中' : '子 Agent 请求记录', provider: '', effort: '待定',
      state: 'delegating', detail: '主会话暂无独立请求记录，下方为子 Agent 请求',
      error: null, time: note.time, current: true, tokens: null, tokensPerSecond: null,
      metricsArePrevious: false, note,
    };
  }

  /** 两行只展示实际请求记录，口径说明保留在卡片悬停提示中。 */
  function renderNotes(notes, sessions = []) {
    const kinds = new Set(['进行中', '完成', '恢复', '取消', '失败', '状态']);
    const records = sessions.flatMap(session => [
      ...session.routes, ...(session.subagents || []), ...(session.completedSubagents || []),
    ]).sort((a, b) => (Date.parse(b.updatedAt) || 0) - (Date.parse(a.updatedAt) || 0));
    const seen = new Set();
    const actual = [...notes, ...records.map(card => card.note)].filter(note => {
      if (!note?.text || !kinds.has(note.kind)) return false;
      const key = JSON.stringify([note.time, note.kind, note.text]);
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    }).slice(0, 2);
    for (let index = 0; index < 2; index++) {
      const note = actual[index];
      byId(`line${index + 1}-text`).closest('.note-row').hidden = !note;
      text(`line${index + 1}-time`, note?.time || '');
      text(`line${index + 1}-kind`, note?.kind || '');
      text(`line${index + 1}-text`, note?.text || '');
    }
  }

  /** 六项数值和可聚焦区域共用完整说明，避免数字节点自己的 title 遮住详情。 */
  function metric(id, valueId, value, detail) {
    window.hudMotion.roll(byId(valueId), value);
    const element = byId(id);
    element.title = detail;
    element.setAttribute('aria-label', detail);
    byId(valueId).title = detail;
  }

  function callCost(value) {
    if (!Number.isFinite(value)) return '—';
    if (value > 0 && value < .01) return '<$0.01';
    return `$${value.toFixed(value < 10 ? 2 : value < 100 ? 1 : 0)}`;
  }

  /** 当日输出速度为 Σout / Σ(ms−ttft_ms)，不把输入或缓存读计为输出吞吐。 */
  function renderStats(stats, scope, costError = snapshot.costError, historical = false) {
    const tokens = compact(stats.tokens);
    text('request-count', `${stats.requests} 次请求`);
    metric('tokens-metric', 'daily-tokens', tokens, `Token · ${scope}\n当日 ${stats.tokens.toLocaleString('zh-CN')} Token；输入 + 输出 + 缓存读取，不含内部分类调用。`);
    text('cost-status', costError ? (stats.cost == null ? '未读取' : '旧读数') : '');
    text('cost-unit', stats.cost == null ? '' : 'USD');
    text('cost-coverage', stats.cost == null ? (stats.unpricedRequests ? `${stats.unpricedRequests} 条未定价` : '等待 Magpie 费用') : `${stats.pricedRequests} 条已定价${stats.unpricedRequests ? ` · ${stats.unpricedRequests} 条未定价` : ''}${costError ? ' · 旧读数' : ''}`);
    metric('cost-metric', 'daily-cost', callCost(stats.cost),
      `费用估值 · ${scope}\n${stats.cost == null ? '未定价' : `$${stats.cost.toFixed(6)} USD`}\n${costError || 'Magpie 当日美元等价费用，含内部分类调用；订阅账号为用量估值。'}\n${stats.pricedRequests} 次已定价，${stats.unpricedRequests} 次未定价。`);
    metric('speed-metric', 'daily-speed', Number.isFinite(stats.tokensPerSecond) ? compactFormat.format(stats.tokensPerSecond).replace('K', 'k') : '—',
      `TPS · ${scope}\n${Number.isFinite(stats.tokensPerSecond) ? `${stats.tokensPerSecond.toFixed(3)} tok/s\n` : ''}${historical ? '所选日期全部有效请求' : sessionSpeedScope(scope)}加权输出速度 = Σ输出 Token / Σ(总耗时 − 首字耗时)；${stats.speedSamples} 条有效记录，不包含等待首字的时间。`);
    const cache = stats.cacheHitRate;
    metric('cache-metric', 'daily-cache', Number.isFinite(cache) ? `${(cache * 100).toFixed(1)}%` : '—',
      `缓存命中率 · ${scope}\n${Number.isFinite(cache) ? `${(cache * 100).toFixed(3)}%` : '暂无有效缓存输入'}\n缓存读取 Token / (输入 Token + 缓存读取 Token)，按完成请求加权；不含输出与内部分类调用。`);
    metric('calls-metric', 'daily-calls', String(stats.requests),
      `调用次数 · ${scope}\n当日 ${stats.requests} 次已结束调用，含主会话和子 Agent，不含内部分类调用。`);
    const average = stats.costPerCall;
    metric('average-metric', 'daily-average', callCost(average), `单次调用费用 · ${scope}\n${Number.isFinite(average)
      ? `$${average.toFixed(8)} USD / 次 = Magpie 费用 / ${stats.pricedRequests} 次已定价调用（含内部分类），不计未定价调用。${costError ? '\n费用为旧读数' : ''}`
      : '暂无可用的已定价调用，等待 Magpie 费用读数。'}`);
    text('stats-scope', scope);
  }

  function sessionSpeedScope(scope) {
    return scope === '全部会话' ? '当日全部有效请求' : '当前会话';
  }

  /** 日志和实时请求共用来源别名，避免同一应用被重复列出。 */
  function sourceName(agent) {
    const name = String(agent || '').trim();
    const key = name.toLowerCase().replace(/[\s_-]/g, '');
    if (key === 'claudedesktop') return 'Claude Desktop';
    if (key === 'claude' || key === 'claudecode') return 'Claude Code';
    if (key === 'codex') return 'Codex';
    return name || 'Agent 未提供';
  }

  function sourceIcon(agent, className = 'source-icon') {
    const icon = document.createElement('span');
    icon.className = className;
    icon.setAttribute('aria-hidden', 'true');
    const family = /claude[\s-]*code/i.test(agent) ? 'claudecode-color' : /claude/i.test(agent) ? 'claude-color' : modelIcon(agent);
    if (family) {
      const image = document.createElement('img');
      image.src = `icons/${family}.svg`;
      image.alt = '';
      icon.append(image);
    } else icon.textContent = agent === '*' ? '∗' : '↗';
    return icon;
  }

  function sourceKey(source) {
    return JSON.stringify([sourceName(source.agent), source.callerKeyId || source.callerKeyName || '']);
  }

  function callerName(source) {
    return (source.callerKeyId && snapshot?.callerKeyNames?.[source.callerKeyId]) || source.callerKeyName || '本机';
  }

  function sourceLabel(source) {
    return [sourceName(source.agent), callerName(source)].join(' · ');
  }

  function selectedSource() {
    return sessionsForView().find(source => sourceKey(source) === selectedAgent)
      || (snapshot.agentStats || []).find(source => sourceKey(source) === selectedAgent);
  }

  function filterAgent(sessions) {
    return selectedAgent === '*' ? sessions : sessions.filter(session => sourceKey(session) === selectedAgent);
  }

  /** 从完整分组累计可加量，再计算加权比例；未知费用不以全局费用补齐。 */
  function scopedDaily(groups) {
    const result = { hourly: Array.from({ length: 24 }, () => ({ tokens: 0, cost: null, costPartial: false })) };
    const sums = ['requests', 'tokens', 'pricedRequests', 'unpricedRequests', 'outputTokens', 'generationMs', 'speedSamples', 'cacheReadTokens', 'cacheDenominatorTokens'];
    sums.forEach(key => { result[key] = groups.reduce((sum, group) => sum + (group.daily[key] || 0), 0); });
    const priced = groups.filter(group => group.daily.requests > 0);
    result.cost = priced.length && priced.every(group => Number.isFinite(group.daily.cost))
      ? priced.reduce((sum, group) => sum + group.daily.cost, 0) : priced.length ? null : 0;
    result.costPerCall = result.cost !== null && result.pricedRequests > 0 ? result.cost / result.pricedRequests : null;
    result.tokensPerSecond = result.generationMs > 0 ? result.outputTokens * 1000 / result.generationMs : null;
    result.cacheHitRate = result.cacheDenominatorTokens > 0 ? result.cacheReadTokens / result.cacheDenominatorTokens : null;
    result.hourly.forEach((hour, index) => {
      hour.tokens = groups.reduce((sum, group) => sum + (group.daily.hourly?.[index]?.tokens || 0), 0);
      const values = priced.map(group => group.daily.hourly?.[index]);
      hour.cost = !values.length ? 0 : values.every(value => Number.isFinite(value?.cost)) ? values.reduce((sum, value) => sum + value.cost, 0) : null;
      hour.costPartial = values.some(value => value?.costPartial);
    });
    return result;
  }

  function statsForSource(view) {
    return selectedAgent === '*' ? view.daily : scopedDaily(filterAgent(view.agentStats || []));
  }

  function statsScope() {
    return selectedAgent === '*' ? '全部会话' : sourceLabel(selectedSource() || { agent: 'Agent 未提供' });
  }

  /** 菜单只按已知 Agent 身份筛选；接口未提供来源 IP 时不推断设备。 */
  function updateAgentSelector(sessions) {
    const sources = new Map();
    [...sessions, ...(snapshot.agentStats || [])].forEach(source => sources.set(sourceKey(source), source));
    const agents = [...sources.keys()].sort((a, b) => sourceLabel(sources.get(a)).localeCompare(sourceLabel(sources.get(b))));
    if (!sources.has(selectedAgent)) selectedAgent = '*';
    const key = JSON.stringify([selectedAgent, agents.map(key => [key, callerName(sources.get(key))])]);
    if (key === agentOptionsKey) return;
    const focusedAgent = agentOptions.contains(document.activeElement) ? document.activeElement.dataset.agent : null;
    agentOptionsKey = key;
    const values = ['*', ...agents];
    agentOptions.replaceChildren(...values.map((agent, index) => {
      const option = document.createElement('button');
      option.type = 'button';
      option.id = `agent-option-${index}`;
      option.className = 'agent-option';
      option.dataset.agent = agent;
      option.setAttribute('role', 'option');
      option.setAttribute('aria-selected', String(agent === selectedAgent));
      option.tabIndex = agent === selectedAgent ? 0 : -1;
      const label = document.createElement('span');
      label.className = 'agent-option-name';
      const source = sources.get(agent);
      const name = agent === '*' ? '全部' : sourceName(source.agent);
      const caller = source ? callerName(source) : '本机';
      label.textContent = name;
      option.title = agent === '*' ? '全部 Agent 的统计、实时路由与额度' : `${name}\n网关密钥：${caller}`;
      const check = document.createElement('span');
      check.className = 'agent-check';
      check.setAttribute('aria-hidden', 'true');
      check.textContent = agent === selectedAgent ? '✓' : '';
      option.append(sourceIcon(agent === '*' ? '*' : name, 'agent-icon'), label);
      if (agent !== '*') {
        const address = document.createElement('small');
        address.className = 'agent-option-address';
        address.textContent = caller;
        option.append(address);
      }
      option.append(check);
      return option;
    }));
    const selected = sources.get(selectedAgent);
    agentSelector.querySelector('.agent-icon').replaceWith(sourceIcon(selected ? sourceName(selected.agent) : '*', 'agent-icon'));
    agentSelector.querySelector('.agent-value').textContent = selected ? sourceLabel(selected) : '全部';
    agentSelector.title = selected ? sourceLabel(selected) : 'Agent：全部';
    if (focusedAgent !== null && !agentOptions.hidden) {
      const option = [...agentOptions.children].find(element => element.dataset.agent === focusedAgent)
        || agentOptions.querySelector('[aria-selected="true"]');
      option.focus();
    }
  }

  function positionAgentOptions() {
    const bounds = agentSelector.getBoundingClientRect();
    agentOptions.style.width = `${Math.min(Math.max(224, bounds.width), innerWidth - 16)}px`;
    agentOptions.style.left = `${Math.max(8, Math.min(bounds.left, innerWidth - agentOptions.offsetWidth - 8))}px`;
    agentOptions.style.top = `${bounds.bottom + 5}px`;
    agentOptions.style.maxHeight = `${Math.max(40, innerHeight - bounds.bottom - 13)}px`;
  }

  function closeAgentOptions(restoreFocus = false) {
    agentOptions.hidden = true;
    agentSelector.setAttribute('aria-expanded', 'false');
    if (restoreFocus) agentSelector.focus();
  }

  function openAgentOptions() {
    if (!snapshot) return;
    agentOptions.hidden = false;
    agentSelector.setAttribute('aria-expanded', 'true');
    positionAgentOptions();
    agentOptions.querySelector('[aria-selected="true"]').focus();
  }

  function chooseAgent(agent) {
    selectedAgent = agent;
    selector.value = '*';
    selectedCard = null;
    renderedIds.clear();
    quotaOrderTimer = clearTimeout(quotaOrderTimer);
    updateSelector(sessionsForView());
    closeAgentOptions(true);
    render(false);
  }

  /** 来源按真实 Agent 合并，图标与 Magpie 上下对齐。 */
  function renderSources(sessions) {
    const agents = new Map();
    for (const session of sessions) {
      const key = sourceKey(session);
      agents.set(key, { source: session, active: Boolean(agents.get(key)?.active) || sessionIsActive(session) });
    }
    const list = byId('source-list');
    for (const [agent, entry] of sourceViews) {
      if (!agents.has(agent)) { entry.remove(); sourceViews.delete(agent); }
    }
    for (const [key, { source, active }] of agents) {
      const agent = sourceName(source.agent);
      const caller = callerName(source);
      let entry = sourceViews.get(key);
      if (entry) {
        entry.dataset.active = String(active);
        entry.querySelector('.source-address').textContent = caller;
        entry.querySelector('.source-address').title = `网关密钥：${caller}`;
        continue;
      }
      entry = document.createElement('div');
      entry.className = 'source-entry';
      entry.dataset.source = key;
      entry.dataset.active = String(active);
      const heading = document.createElement('div');
      heading.className = 'source-heading';
      const icon = sourceIcon(agent);
      const name = document.createElement('strong');
      name.textContent = agent;
      name.title = agent;
      const copy = document.createElement('span');
      copy.className = 'source-copy';
      const address = document.createElement('small');
      address.className = 'source-address';
      address.textContent = caller;
      address.title = `网关密钥：${caller}`;
      copy.append(name, address);
      heading.append(icon, copy);
      entry.append(heading);
      if (!sourceViews.size) list.replaceChildren();
      sourceViews.set(key, entry);
      list.append(entry);
    }
    if (!agents.size) list.textContent = '等待请求';
    list.dataset.count = String(agents.size);
    hud.dataset.sources = agents.size <= 2 ? 'compact' : 'multiple';
  }

  /** 按当前会话选择决定额度展示项；“最近会话”汇总当天全部会话。 */
  function renderQuotas(sessions, forceOrder) {
    const list = byId('quota-list');
    const before = window.hudMotion.capture([...quotaViews.values()].map(view => view.item), node => node.dataset.quotaKey);
    const scroll = list.scrollTop;
    const accounts = new Map();
    sessions.flatMap(session => [...(session.quotaRoutes || []), ...session.routes, ...(session.subagents || [])]).forEach(card => {
      const key = JSON.stringify([card.provider.toLowerCase(), card.account]);
      if (!accounts.has(key)) accounts.set(key, { card, models: new Set() });
      accounts.get(key).models.add(card.model);
    });
    const entries = [];
    const readings = [];
    for (const [key, { card, models }] of accounts) {
      const candidates = (snapshot.quotas || []).filter(quota =>
        quota.provider.toLowerCase() === card.provider.toLowerCase() &&
        (quota.user ? quota.user === card.account
          : quota.kind === 'balance' && quota.name === card.account
            || quota.kind === 'plan' && quota.provider === 'opencode-go' && card.account === 'opencode.ai'));
      if (candidates.length !== 1) continue;
      const quota = candidates[0];
      const read = quota.readAt || quota.asOf;
      if (read && !Number.isNaN(Date.parse(read))) readings.push(new Date(read));
      const windows = quota.windows.length ? [...quota.windows].sort((a, b) => windowRank(a) - windowRank(b)) : [null];
      entries.push({ key, card, models, quota, windows });
    }
    const orderKey = entries.map(entry => entry.key).join('\n');
    if (!forceOrder && quotaRenderKey && orderKey !== quotaRenderKey) {
      const ranks = new Map(quotaRenderKey.split('\n').map((key, index) => [key, index]));
      entries.sort((a, b) => (ranks.get(a.key) ?? Infinity) - (ranks.get(b.key) ?? Infinity));
    } else quotaRenderKey = orderKey;
    const next = new Map(entries.map(entry => [entry.key, entry]));
    for (const [key, view] of quotaViews) {
      if (!next.has(key)) {
        if (quotaTooltipView === view) hideQuotaTooltip();
        view.item.remove();
        quotaViews.delete(key);
      }
    }
    let previous = null;
    for (const entry of entries) {
      const itemKey = entry.key;
      const view = quotaViews.get(itemKey) || createQuotaItem();
      quotaViews.set(itemKey, view);
      view.item.dataset.quotaKey = itemKey;
      view.entry = entry;
      view.carouselStart ??= performance.now();
      updateQuotaWindow(view);
      if (view.item.parentElement !== list || view.item.previousElementSibling !== previous) {
        list.insertBefore(view.item, previous ? previous.nextSibling : list.firstChild);
      }
      previous = view.item;
    }
    let empty = list.querySelector('.quota-empty');
    if (!entries.length && !empty) {
      empty = document.createElement('p');
      empty.className = 'quota-empty';
      empty.textContent = '暂无额度';
      list.append(empty);
    } else if (entries.length && empty) empty.remove();
    // 整批读取失败才标整栏旧数据；单个账号 401 只标该行，不连累其它账号。
    const quotaFailed = Boolean(snapshot.quotaError);
    const staleRows = [...quotaViews.values()].filter(view => view.item.dataset.stale === 'true').length;
    list.title = [quotaFailed ? '旧读数' : staleRows ? `${staleRows} 个账号读数失败` : '',
      snapshot.quotaError || (readings.length ? `最近读数 ${new Date(Math.min(...readings)).toLocaleString('zh-CN')}` : '暂无额度读数')].filter(Boolean).join('\n');
    const freshness = byId('quota-freshness');
    freshness.hidden = !quotaFailed;
    freshness.title = list.title;
    list.scrollTop = scroll;
    updateQuotaClocks();
    window.hudMotion.layout(before, [...quotaViews.values()].map(view => view.item), node => node.dataset.quotaKey);
  }

  /** 同一账号只展示最短窗口；无法识别时排在已知窗口之后。 */
  function windowRank(span) {
    const name = (span?.name || '').trim().toLowerCase();
    if (/^(weekly|week|周|每周)$/.test(name)) return 7 * 1440;
    if (/^(daily|day|日|每天)$/.test(name)) return 1440;
    if (/^(monthly|month|月|每月)$/.test(name)) return 30 * 1440;
    const match = name.match(/(\d+(?:\.\d+)?)\s*(months?|weeks?|minutes?|mins?|hours?|hrs?|days?|[mhdw])\b/i)
      || name.match(/(\d+(?:\.\d+)?)\s*(分钟|小时|天|周|月)/);
    if (!match) return Infinity;
    const unit = match[2];
    const scale = /^(months?|月)$/.test(unit) ? 43200 : /^(weeks?|w|周)$/.test(unit) ? 10080
      : /^(days?|d|天)$/.test(unit) ? 1440 : /^(hours?|hrs?|h|小时)$/.test(unit) ? 60 : 1;
    return Number(match[1]) * scale;
  }

  /** 在同一根条内切换窗口，账号行与色条节点始终复用。 */
  function updateQuotaWindow(view, now = performance.now()) {
    const { windows } = view.entry;
    const phase = (now - view.carouselStart) % (20_000 + (windows.length - 1) * 5_000);
    const index = phase < 20_000 ? 0 : 1 + Math.floor((phase - 20_000) / 5_000);
    const span = windows[index];
    const name = span?.name || '额度';
    const zcodeWindow = view.entry.quota.provider.toLowerCase() === 'zcode'
      ? name.match(/^ZCode\s+(.+?)\s*[·•]\s*(.+)$/i) : null;
    const windowName = zcodeWindow ? `${zcodeWindow[1].replace(/\s+Dev Day$/i, '').replace(/\s+Plan$/i, '')} · ${zcodeWindow[2].replace(/^GLM-/i, '').replace(/-Flash$/i, 'F')}`
      : /^5\s*(hours?|h)$/i.test(name) ? '5h'
      : /^(weekly|week|7 days|周|每周)$/i.test(name) ? '周'
        : /^(monthly|month|月|每月)$/i.test(name) ? '月' : name;
    view.windowIndex = index;
    updateQuotaItem(view, { ...view.entry, span, windowName });
  }

  /** 本地轮播不重排列表，也不触发接口请求。 */
  function quotaCarousel() {
    const now = performance.now();
    for (const view of quotaViews.values()) updateQuotaWindow(view, now);
  }

  /** 固定周期的时间进度只在本地推进，不触发额度接口或重建卡片。 */
  function updateQuotaClocks() {
    const now = Date.now();
    for (const view of quotaViews.values()) {
      const { periodMs, resetAt } = view;
      view.marker.hidden = !Number.isFinite(periodMs) || periodMs <= 0 || !Number.isFinite(resetAt);
      if (view.marker.hidden) continue;
      const elapsed = Math.min(100, Math.max(0, (1 - (resetAt - now) / periodMs) * 100));
      slideQuota(view.marker, 'left', elapsed);
      const used = view.track.dataset.unknown === 'false' ? Number(view.fill.dataset.percent) : NaN;
      const difference = used - elapsed;
      const pace = !Number.isFinite(used) ? 'unknown' : difference > 5 ? 'ahead' : difference < -5 ? 'within' : 'balanced';
      view.marker.dataset.pace = pace;
      const comparison = pace === 'ahead' ? '用量高于时间进度超过 5 个百分点' : pace === 'within' ? '用量低于时间进度超过 5 个百分点' : pace === 'balanced' ? '用量与时间进度相差不超过 5 个百分点' : '用量未知';
      const hint = `细竖线：周期已过 ${elapsed.toFixed(1)}%${view.monthEstimated ? '（按一个月估算：从下次重置时间倒推一个日历月）' : ''}；色条：已用额度；${comparison}`;
      view.track.setAttribute('aria-description', hint);
    }
  }

  /** 额度使用供应商品牌，不根据供应商背后的模型猜测图标。 */
  function quotaIcon(provider) {
    const name = provider.toLowerCase().replace(/[-_\s]/g, '');
    if (['codex', 'chatgpt', 'chatgptapi', 'openai'].includes(name)) return 'openai';
    if (name.startsWith('workbuddy')) return 'workbuddy-color';
    if (name === 'grok') return 'xai';
    if (name === 'gemini') return 'geminicli-color';
    if (name === 'copilot') return 'githubcopilot';
    if (name === 'zcode') return 'zcode';
    return modelIcon(provider);
  }

  /** 同一窗口的数值和文字共用口径；缺失读数不伪装为零。 */
  function quotaWindowDetails(span) {
    const used = span.unlimited ? null : Number.isFinite(span.used) ? span.used
      : Number.isFinite(span.remaining) ? 100 - span.remaining : null;
    const percent = used === null ? null : Math.min(100, Math.max(0, used));
    const format = value => value.toLocaleString('zh-CN', { maximumFractionDigits: 2 });
    const status = span.unlimited ? '不限量' : used === null ? '用量未提供' : used >= 100 ? `已用 ${format(used)}% · 已用尽` : `已用 ${format(used)}%`;
    const severity = used === null ? 'unknown' : used > 95 ? 'critical' : used > 80 ? 'warning' : 'normal';
    const amount = Number.isFinite(span.amount) ? span.amount
      : Number.isFinite(span.limit) && used !== null ? span.limit * used / 100 : null;
    const credits = /credits?|积分/i.test(`${span.name || ''} ${span.unit || ''}`);
    const unit = credits ? ' 积分' : span.unit ? ` ${span.unit}` : '';
    const quantity = amount !== null && Number.isFinite(span.limit)
      ? `已用 ${format(amount)} / ${format(span.limit)}${unit}` : span.display || '';
    const remaining = amount !== null && Number.isFinite(span.limit)
      ? `剩余 ${format(Math.max(0, span.limit - amount))}${unit}`
      : used !== null ? `剩余 ${format(Math.max(0, 100 - used))}%` : '';
    const reset = span.resetsAt && !Number.isNaN(Date.parse(span.resetsAt))
      ? `${new Date(span.resetsAt).toLocaleString('zh-CN', { hour12: false })} 重置` : '重置时间未提供';
    const name = [span.family || span.pool, span.name || '额度'].filter(Boolean).join(' · ');
    return { used, percent, status, severity, quantity, remaining, reset, name };
  }

  /** 只在悬停或聚焦的账号上展开；渲染仍只使用 textContent。 */
  function renderQuotaTooltip(view) {
    const { card, models, quota, windows } = view.entry;
    const key = JSON.stringify([quota, card.account, [...models], snapshot.quotaError]);
    if (key === quotaTooltipKey) return;
    quotaTooltipKey = key;
    const node = (tag, className, text) => {
      const element = document.createElement(tag);
      element.className = className;
      if (text) element.textContent = text;
      return element;
    };
    const header = node('div', 'quota-tooltip-heading');
    const identity = node('div', 'quota-tooltip-identity');
    identity.append(node('strong', '', quota.name || quota.provider), node('span', 'quota-tooltip-account', card.account));
    const count = quota.windows.length;
    header.append(identity, node('span', 'quota-tooltip-count', `${count} 个窗口`));
    const content = [header];
    const context = [quota.plan, [...models].join(' · ')].filter(Boolean).join(' · ');
    if (context) content.push(node('p', 'quota-tooltip-context', context));
    const list = node('div', 'quota-tooltip-windows');
    for (const span of windows.filter(Boolean)) {
      const details = quotaWindowDetails(span);
      const row = node('div', 'quota-tooltip-window');
      row.dataset.severity = details.severity;
      const heading = node('div', 'quota-tooltip-window-heading');
      heading.append(node('strong', '', details.name), node('span', 'quota-tooltip-value', details.status));
      const track = node('div', 'quota-tooltip-track');
      track.dataset.unknown = String(details.percent === null);
      if (details.percent !== null) {
        track.setAttribute('role', 'progressbar');
        track.setAttribute('aria-label', details.name);
        track.setAttribute('aria-valuemin', '0');
        track.setAttribute('aria-valuemax', '100');
        track.setAttribute('aria-valuenow', String(details.percent));
        track.setAttribute('aria-valuetext', [details.status, details.quantity, details.remaining].filter(Boolean).join('，'));
        const fill = node('span', 'quota-tooltip-fill');
        fill.style.width = `${details.percent}%`;
        track.append(fill);
      }
      row.append(heading, track);
      if (details.quantity) row.append(node('div', 'quota-tooltip-quantity', details.quantity));
      const meta = node('div', 'quota-tooltip-meta');
      if (details.remaining) meta.append(node('span', '', details.remaining));
      meta.append(node('time', '', details.reset));
      row.append(meta);
      list.append(row);
    }
    if (count) content.push(list);
    else if (!quota.balance) content.push(node('p', 'quota-tooltip-context', '未提供额度窗口'));
    if (quota.balance) content.push(node('p', 'quota-tooltip-balance', `余额 ${quota.balance}`));
    const error = snapshot.quotaError || quota.error;
    if (error) content.push(node('p', 'quota-tooltip-error', `旧读数 · ${error}`));
    const read = quota.readAt || quota.asOf;
    if (read) {
      const time = !Number.isNaN(Date.parse(read)) ? new Date(read).toLocaleString('zh-CN', { hour12: false }) : read;
      content.push(node('div', 'quota-tooltip-footer', `最近读数 ${time}`));
    }
    const scroll = quotaTooltip.scrollTop;
    quotaTooltip.replaceChildren(...content);
    quotaTooltip.scrollTop = scroll;
  }

  /** 浮层挂在 body 上避开侧栏裁剪，始终收在当前窗口内。 */
  function positionQuotaTooltip() {
    if (!quotaTooltipView || quotaTooltip.hidden) return;
    const anchor = quotaTooltipView.item.getBoundingClientRect();
    const width = quotaTooltip.offsetWidth;
    const height = quotaTooltip.offsetHeight;
    let left = anchor.right + 12;
    if (left + width > innerWidth - 8) left = anchor.left - width - 12;
    if (left < 8) {
      left = Math.min(anchor.left, innerWidth - width - 8);
      const top = anchor.bottom + 8;
      quotaTooltip.style.top = `${Math.max(8, Math.min(top + height <= innerHeight - 8 ? top : anchor.top - height - 8, innerHeight - height - 8))}px`;
    } else quotaTooltip.style.top = `${Math.max(8, Math.min(anchor.top - 8, innerHeight - height - 8))}px`;
    quotaTooltip.style.left = `${Math.max(8, left)}px`;
  }

  function hideQuotaTooltip() {
    clearTimeout(quotaTooltipShowTimer);
    clearTimeout(quotaTooltipHideTimer);
    quotaTooltipView?.item.removeAttribute('aria-describedby');
    quotaTooltipView = null;
    quotaTooltip.hidden = true;
  }

  function showQuotaTooltip(view, immediate = false) {
    clearTimeout(quotaTooltipShowTimer);
    clearTimeout(quotaTooltipHideTimer);
    const show = () => {
      if (!view.item.isConnected) return;
      if (quotaTooltipView !== view) {
        quotaTooltipView?.item.removeAttribute('aria-describedby');
        quotaTooltip.scrollTop = 0;
      }
      quotaTooltipView = view;
      renderQuotaTooltip(view);
      quotaTooltip.hidden = false;
      view.item.setAttribute('aria-describedby', 'quota-tooltip');
      positionQuotaTooltip();
    };
    if (immediate || !quotaTooltip.hidden) show();
    else quotaTooltipShowTimer = setTimeout(show, 180);
  }

  function scheduleQuotaTooltipHide() {
    clearTimeout(quotaTooltipShowTimer);
    clearTimeout(quotaTooltipHideTimer);
    quotaTooltipHideTimer = setTimeout(() => {
      if (!quotaTooltip.matches(':hover') && !quotaTooltipView?.item.matches(':hover, :focus')) hideQuotaTooltip();
    }, 160);
  }

  /** 只创建稳定元素，不在每次快照中重排。 */
  function createQuotaItem() {
    const item = document.createElement('div');
    item.className = 'quota-item';
    item.tabIndex = 0;
    const icon = document.createElement('span');
    icon.className = 'quota-icon';
    const image = document.createElement('img');
    image.alt = '';
    image.hidden = true;
    const fallback = document.createElement('span');
    fallback.textContent = '◇';
    fallback.setAttribute('aria-hidden', 'true');
    icon.append(image, fallback);
    const header = document.createElement('div');
    header.className = 'quota-meta';
    const account = document.createElement('span');
    account.className = 'quota-account';
    const reset = document.createElement('time');
    reset.className = 'quota-reset';
    header.append(account, reset);
    const row = document.createElement('div');
    row.className = 'quota-window';
    const label = document.createElement('span');
    label.className = 'quota-period';
    const track = document.createElement('div');
    track.className = 'quota-track';
    track.setAttribute('role', 'progressbar');
    const fill = document.createElement('span');
    fill.className = 'quota-fill';
    const marker = document.createElement('span');
    marker.className = 'quota-time-marker';
    marker.hidden = true;
    marker.setAttribute('aria-hidden', 'true');
    track.append(fill, marker);
    const value = document.createElement('strong');
    row.append(label, track, value);
    item.append(icon, header, row);
    const view = { item, icon, image, fallback, account, reset, label, track, fill, marker, value };
    item.addEventListener('pointerenter', () => showQuotaTooltip(view));
    item.addEventListener('pointerleave', scheduleQuotaTooltipHide);
    item.addEventListener('focus', () => showQuotaTooltip(view, true));
    item.addEventListener('blur', scheduleQuotaTooltipHide);
    return view;
  }

  /** 重置倒计时使用两级单位；不足一分钟时不显示负数或虚假的零分钟。 */
  function resetRemaining(resetTime, now = Date.now()) {
    if (!resetTime) return '—';
    const remaining = resetTime.getTime() - now;
    if (remaining <= 0) return '已到期';
    const minutes = Math.floor(remaining / 60_000);
    if (minutes >= 1440) return `${Math.floor(minutes / 1440)}d${Math.floor(minutes % 1440 / 60)}h`;
    if (minutes >= 60) return `${Math.floor(minutes / 60)}h${minutes % 60}m`;
    return minutes ? `${minutes}m` : '<1m';
  }

  /** 色条和周期线从当前位置滑到新位置；减少动态时直接落位。 */
  function slideQuota(node, property, percent) {
    const shown = property === 'width' ? percent.toFixed(1) : percent.toFixed(2);
    const target = property === 'width' ? `${shown}%` : `clamp(0px, ${shown}%, calc(100% - 1px))`;
    if (node.dataset.shown === shown) return;
    const first = node.dataset.shown === undefined;
    node.dataset.shown = shown;
    if (!window.hudMotion.enabled() || first) { node.style[property] = target; return; }
    const previous = getComputedStyle(node)[property];
    node.getAnimations().forEach(animation => animation.cancel());
    node.style[property] = target;
    window.hudMotion.animate(node, [{ [property]: previous }, { [property]: target }], 480);
  }

  /** 更新同一额度项的数值和提示，不改变其在列表中的位置。 */
  function updateQuotaItem(view, { card, models, quota, span, windowName }) {
    view.item.dataset.stale = String(Boolean(quota.error));
    let stale = view.item.querySelector('.quota-stale');
    if (quota.error && !stale) {
      stale = document.createElement('span');
      stale.className = 'quota-stale';
      stale.textContent = '旧读数';
      view.account.after(stale);
    } else if (!quota.error && stale) stale.remove();
    if (stale) stale.removeAttribute('title');
    const family = quotaIcon(quota.provider);
    view.item.dataset.namedWindow = String(quota.provider.toLowerCase() === 'zcode');
    const iconPath = family ? `icons/${family}.${family === 'zcode' ? 'png' : 'svg'}` : '';
    if (family && view.image.getAttribute('src') !== iconPath) view.image.src = iconPath;
    view.image.hidden = !family;
    view.fallback.hidden = Boolean(family);
    view.image.alt = quota.name || quota.provider;
    view.account.textContent = !quota.user && quota.kind === 'plan' ? quota.name || card.account : shortAccount(card.account);
    view.account.title = card.account;
    const resetTime = span?.resetsAt && !Number.isNaN(Date.parse(span.resetsAt)) ? new Date(span.resetsAt) : null;
    const credits = /credits?|积分/i.test(`${span?.name || ''} ${span?.unit || ''}`);
    const period = windowRank(span);
    // 月窗口按下次重置时间倒推一个日历月；月末夹到上月最后一天，并明确标为估算。
    const calendarMonth = /^(months?|monthly|月|每月|1\s*(months?|月))$/i.test((span?.name || '').trim());
    view.monthEstimated = calendarMonth;
    let monthPeriod = NaN;
    if (calendarMonth && resetTime) {
      const start = new Date(resetTime);
      const day = start.getUTCDate();
      start.setUTCDate(1);
      start.setUTCMonth(start.getUTCMonth() - 1);
      const lastDay = new Date(Date.UTC(start.getUTCFullYear(), start.getUTCMonth() + 1, 0)).getUTCDate();
      start.setUTCDate(Math.min(day, lastDay));
      monthPeriod = resetTime.getTime() - start.getTime();
    }
    view.periodMs = !credits && !span?.unlimited
      ? calendarMonth ? monthPeriod : !/months?|月/i.test(span?.name || '') && Number.isFinite(period) ? period * 60_000 : NaN : NaN;
    view.resetAt = resetTime ? resetTime.getTime() : NaN;
    if (!Number.isFinite(view.periodMs) || !Number.isFinite(view.resetAt)) view.track.removeAttribute('aria-description');
    let remainingCredits = null;
    if (credits && Number.isFinite(span.limit)) {
      if (Number.isFinite(span.amount)) remainingCredits = Math.max(0, span.limit - span.amount);
      else if (Number.isFinite(span.remaining)) remainingCredits = Math.max(0, span.limit * span.remaining / 100);
      else if (Number.isFinite(span.used)) remainingCredits = Math.max(0, span.limit * (1 - span.used / 100));
    }
    view.reset.textContent = credits ? remainingCredits === null ? '余量未知' : `余 ${Number(remainingCredits.toFixed(2)).toLocaleString('zh-CN')}` : resetRemaining(resetTime);
    view.reset.dateTime = !credits && resetTime ? span.resetsAt : '';
    view.reset.title = resetTime ? `${resetTime.toLocaleString('zh-CN', { hour12: false })} 重置` : '重置时间未提供';
    view.label.textContent = credits ? '积分' : windowName;
    const used = span && !span.unlimited ? (Number.isFinite(span.used) ? span.used : Number.isFinite(span.remaining) ? 100 - span.remaining : null) : null;
    const percent = used === null ? null : Math.min(100, Math.max(0, used));
    const summary = percent !== null ? `已用 ${Number.isInteger(percent) ? percent : percent.toFixed(1)}%` : span?.unlimited ? '不限量' : quota.balance || '未提供';
    view.value.textContent = percent !== null ? `${Number.isInteger(percent) ? percent : percent.toFixed(1)}%` : span?.unlimited ? '∞' : '—';
    view.track.setAttribute('aria-label', `${card.account} ${windowName} 已用额度`);
    view.track.setAttribute('aria-valuetext', summary);
    view.track.dataset.unknown = String(percent === null);
    if (percent !== null) {
      view.track.setAttribute('aria-valuemin', '0');
      view.track.setAttribute('aria-valuemax', '100');
      view.track.setAttribute('aria-valuenow', String(percent));
      view.fill.dataset.percent = String(percent);
      slideQuota(view.fill, 'width', percent);
    } else {
      slideQuota(view.fill, 'width', 0);
      view.fill.dataset.percent = '0';
      view.track.removeAttribute('aria-valuenow');
    }
    const severity = percent === null ? 'unknown' : used > 95 ? 'critical' : used > 80 ? 'warning' : 'normal';
    view.track.parentElement.dataset.severity = severity;
    view.track.parentElement.dataset.low = String(severity === 'warning');
    const read = quota.readAt || quota.asOf;
    const windows = (quota.windows || []).map(span => {
      const details = quotaWindowDetails(span);
      return [details.name, details.status, details.quantity, details.remaining, details.reset].filter(Boolean).join('，');
    });
    const readTime = read && !Number.isNaN(Date.parse(read)) ? new Date(read).toLocaleString('zh-CN', { hour12: false }) : read;
    const description = [quota.name || quota.provider, card.account, [...models].join(' · '), ...windows,
      quota.balance ? `余额 ${quota.balance}` : '', readTime ? `读数 ${readTime}` : '', snapshot.quotaError || quota.error || ''].filter(Boolean).join('，');
    // 系统 title 会遮住自绘浮层；完整口径保留在可访问名称中。
    [view.item, view.icon, view.account, view.reset, view.track, view.marker, view.value, view.label].forEach(element => element.removeAttribute('title'));
    view.item.title = '';
    view.item.setAttribute('aria-label', description);
    if (quotaTooltipView === view) { renderQuotaTooltip(view); positionQuotaTooltip(); }
  }

  /** 根据当前会话选择生成分支和两行说明，保留滚动位置。 */
  function render(animate) {
    const forceQuotaOrder = !quotaOrderTimer;
    if (!quotaOrderTimer) quotaOrderTimer = setTimeout(() => { quotaOrderTimer = 0; render(false); }, 60_000);
    hud.dataset.layout = innerWidth < 680 ? 'tall' : 'wide';
    if (!snapshot) return;
    const allSessions = filterAgent(sessionsForView().filter(hasSessionContent));
    const visibleSessions = allSessions.filter(item => item.routes.length || subagentsForView(item).length);
    const session = allSessions.find(item => item.id === selector.value);
    const activeCount = (session ? [session] : allSessions).filter(sessionIsActive).length;
    const activeScope = session ? session.label : statsScope();
    const activeHint = `${activeScope}：${activeCount} 个会话正在同时请求（含正在请求的子 Agent，同一会话只计一次）`;
    window.hudMotion.roll(byId('active-session-count'), String(activeCount));
    byId('active-sessions').title = activeHint;
    byId('active-sessions').setAttribute('aria-label', activeHint);
    hud.dataset.scope = session ? 'session' : 'all';
    const { protectedSessions, recent } = orderedSessions(visibleSessions);
    const chooseSessions = () => session ? [session] : [...protectedSessions, ...recent.slice(0, Math.max(0, visibleSessionCapacity() - protectedSessions.length))];
    let sessions = chooseSessions();
    const before = window.hudMotion.capture(routeList.querySelectorAll('.route-card'), node => node.dataset.motionKey);
    const oldScroll = routeList.scrollTop;
    hud.dataset.state = snapshot.state;
    text('watch-label', snapshot.watchLabel);
    byId('watch-label').title = snapshot.error || snapshot.watchLabel;
    text('router-host', snapshot.magpieHost);
    renderSources(sessions);
    window.dispatchEvent(new CustomEvent('hud-live-stats', { detail: {
      daily: session ? session.daily : statsForSource(snapshot),
      date: snapshot.date, earliestDate: snapshot.earliestDate, host: snapshot.magpieHost,
      scope: session ? session.label : statsScope(), costError: snapshot.costError,
      agentScope: selectedAgent, agentScopeLabel: statsScope(),
      projectMonth: view => {
        if (selectedAgent === '*') return view;
        const values = new Map();
        filterAgent(view.agentDays || []).forEach(group => group.days.forEach(day => {
          const sum = values.get(day.date) || { tokens: 0, requests: 0 };
          sum.tokens += day.tokens;
          sum.requests += day.requests;
          values.set(day.date, sum);
        }));
        return { ...view, days: view.days.map(day => ({ date: day.date, tokens: 0, requests: 0, ...values.get(day.date) })) };
      },
      projectHistory: view => ({ ...view, daily: statsForSource(view), scope: statsScope() }),
    } }));
    renderQuotas(session ? [session] : filterAgent(snapshot.sessions), forceQuotaOrder);
    byId('empty-route').querySelector('strong').textContent = selectedAgent === '*' ? '尚无今日会话' : `${statsScope()} 暂无会话`;
    if (!agentOptions.hidden) positionAgentOptions();
    if (hud.dataset.layout === 'tall') {
      sessions = chooseSessions();
      renderSources(sessions);
    }
    const fragment = document.createDocumentFragment();
    const newIds = [];
    const currentIds = new Set();
    const rememberCard = card => {
      currentIds.add(card.id);
      if (!renderedIds.has(card.id)) newIds.push(card.id);
    };
    sessions.forEach(item => {
      const group = document.createElement('div');
      group.className = 'session-group';
      group.dataset.sessionId = item.id;
      const children = subagentsForView(item, Boolean(session));
      const parents = item.routes.length ? item.routes.slice(0, 4) : [parentAnchor(item)];
      parents.forEach((card, index) => {
        const node = createCard(card, item);
        node.dataset.motionKey = JSON.stringify([item.id, 'parent', index === 0 ? 'current' : card.id]);
        group.append(node);
        rememberCard(card);
      });
      if (children.length) {
        group.classList.add('has-subagents');
        const list = document.createElement('div');
        list.className = 'subagent-list';
        list.setAttribute('role', 'group');
        list.setAttribute('aria-label', `${item.label}的${session ? '子 Agent 请求' : '活跃子 Agent'}`);
        children.forEach(card => {
          const node = createSubagent(card, item);
          node.dataset.motionKey = JSON.stringify([item.id, 'child', card.subagentId || card.id]);
          list.append(node);
          rememberCard(card);
        });
        group.append(list);
      }
      fragment.append(group);
    });
    routeList.replaceChildren(fragment);
    routeList.hidden = sessions.length === 0;
    if (!session && hud.dataset.layout === 'wide') {
      const available = byId('route-area').clientHeight;
      while (sessions.length > 1 && routeList.scrollHeight > available) {
        const index = sessions.findLastIndex(item => !sessionIsActive(item));
        if (index < 0) break;
        const [removed] = sessions.splice(index, 1);
        const group = [...routeList.children].find(element => element.dataset.sessionId === removed.id);
        group.querySelectorAll('.route-card').forEach(element => currentIds.delete(element.dataset.cardId));
        group.remove();
      }
    }
    renderSources(sessions);
    text('session-count', session ? session.label : `${visibleSessions.length}会话, 显示${sessions.length}个`);
    byId('source-node').title = [...new Set(sessions.map(sourceLabel))].join('\n');
    routeList.scrollTop = oldScroll;
    byId('empty-route').hidden = sessions.length > 0;
    const inbound = byId('inbound-active');
    const hasRequests = sessions.some(item => item.routes.some(card => ['running', 'waiting'].includes(card.state)) || activeSubagents(item).length);
    inbound.classList.toggle('flowing', hasRequests);
    inbound.style.opacity = sessions.length ? '1' : '0';
    const picked = sessions.flatMap(item => [...item.routes, ...subagentsForView(item, Boolean(session))]).find(card => card.id === selectedCard);
    if (picked) renderNotes([picked.note], sessions.filter(item => item.id === picked.sessionId));
    else { selectedCard = null; renderNotes(session ? session.notes : selectedAgent !== '*' && sessions.length ? sessions[0].notes : snapshot.notes, sessions); }
    renderedIds = currentIds;
    updateCardTimes();
    const moving = window.hudMotion.layout(before, routeList.querySelectorAll('.route-card'), node => node.dataset.motionKey);
    requestDraw(animate ? newIds : []);
    if (moving) followCardMotion();
  }

  /** 至少显示三个会话；窗口更高时按卡片高度增加，活跃会话始终全部保留。 */
  function visibleSessionCapacity() {
    const content = document.querySelector('.content');
    const style = getComputedStyle(content);
    let available = content.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
    const tall = hud.dataset.layout === 'tall';
    if (tall) {
      available -= document.querySelector('.sidebar').offsetHeight + parseFloat(style.rowGap)
        + document.querySelector('.transit').offsetHeight + parseFloat(getComputedStyle(document.querySelector('.routing')).rowGap);
    }
    return Math.max(3, Math.floor((available + 9) / (tall ? 105 : 77)));
  }

  /** 各来源独立连入路由器，复用线节点以保持流动动画相位。 */
  function drawSourceLines() {
    const layer = byId('source-lines');
    const bounds = document.querySelector('.transit').getBoundingClientRect();
    const router = document.querySelector('.router-emblem').getBoundingClientRect();
    layer.setAttribute('viewBox', `0 0 ${bounds.width} ${bounds.height}`);
    const present = new Set();
    let index = 0;
    const sources = [...sourceViews].sort((a, b) => a[1].getBoundingClientRect().top - b[1].getBoundingClientRect().top);
    for (const [agent, entry] of sources) {
      const icon = entry.querySelector('.source-icon').getBoundingClientRect();
      if (!icon.width || icon.bottom <= bounds.top || icon.top >= bounds.bottom) continue;
      present.add(agent);
      let path = sourceBranches.get(agent);
      if (!path) {
        path = document.createElementNS(namespace, 'path');
        path.dataset.source = agent;
        sourceBranches.set(agent, path);
        layer.append(path);
      }
      const stacked = router.top >= icon.bottom;
      index += 1;
      // 纵向来源走图标左侧的紧凑独立弧线，避免穿过下方图标并重合成一根竖线。
      const port = index / (sources.length + 1);
      const x = (stacked ? icon.left : icon.right) - bounds.left;
      const y = icon.top + icon.height / 2 - bounds.top;
      const endX = router.left - bounds.left;
      const endY = router.top + router.height * port - bounds.top;
      const lane = Math.max(1, Math.min(x, endX) - 14) + (index - 1) * Math.min(6, 14 / Math.max(1, sources.length - 1));
      path.setAttribute('d', stacked
        ? `M${x} ${y}C${lane} ${y} ${lane} ${endY} ${endX} ${endY}`
        : `M${x} ${y}C${x + (endX - x) * .5} ${y} ${endX - 12} ${endY} ${endX} ${endY}`);
      const flowing = entry.dataset.active === 'true';
      if (path.dataset.active !== String(flowing)) {
        path.setAttribute('class', `route-active${flowing ? ' flowing' : ''}`);
        path.dataset.active = String(flowing);
      }
    }
    for (const [agent, path] of sourceBranches) {
      if (!present.has(agent)) { path.remove(); sourceBranches.delete(agent); }
    }
  }

  /** 用实际路由块位置画贝塞尔分支，列表滚动和窗口大小变化后重新对齐。 */
  function drawLines(newIds = []) {
    drawSourceLines();
    const layer = lineLayer.getBoundingClientRect();
    const router = document.querySelector('.router-emblem').getBoundingClientRect();
    const node = document.querySelector('.router-node').getBoundingClientRect();
    const area = byId('route-area').getBoundingClientRect();
    const tall = hud.dataset.layout === 'tall';
    // 竖排从节点文字下方引出，避免连线穿过 magpie 名称和地址。
    const startX = (tall ? node.left + node.width / 2 : router.right) - layer.left;
    const origin = (tall ? node.bottom : router.top + router.height / 2) - layer.top;
    const visibleIds = new Set();
    lineLayer.setAttribute('viewBox', `0 0 ${layer.width} ${layer.height}`);
    const parents = [...routeList.querySelectorAll('.route-card:not(.subagent-row)')].map(card => card.getBoundingClientRect());
    const columns = new Map();
    if (tall) parents.forEach(rect => {
      const x = Math.round(rect.left - layer.left) - 14;
      columns.set(x, Math.max(columns.get(x) ?? 0, rect.top + rect.height / 2 - layer.top));
    });
    const busY = Math.max(origin + 12, area.top - layer.top + 12);
    let trunk = lineLayer.querySelector('.branch-trunk');
    if (tall && parents.length) {
      if (!trunk) {
        trunk = document.createElementNS(namespace, 'path');
        trunk.setAttribute('class', 'branch-trunk');
      }
      // 所有列从卡片上方横向分流，列内竖线只占左侧预留走廊，不穿过卡片。
      trunk.setAttribute('d', [...columns].map(([x, end]) =>
        `M${startX} ${origin}C${startX} ${busY} ${x} ${origin} ${x} ${busY + 12}L${x} ${Math.max(end, busY + 12)}`).join(' '));
      if (lineLayer.firstChild !== trunk) lineLayer.insertBefore(trunk, lineLayer.firstChild);
    } else { trunk?.remove(); trunk = null; }
    let previousNode = trunk;
    const place = node => {
      const next = previousNode ? previousNode.nextSibling : lineLayer.firstChild;
      if (next !== node) lineLayer.insertBefore(node, next);
      previousNode = node;
    };
    routeList.querySelectorAll('.route-card').forEach(card => {
      const rectangle = card.getBoundingClientRect();
      const x = rectangle.left - layer.left;
      const y = rectangle.top + rectangle.height / 2 - layer.top;
      const id = card.classList.contains('subagent-row')
        ? JSON.stringify([card.closest('.session-group').dataset.sessionId, card.dataset.subagentId]) : card.dataset.cardId;
      visibleIds.add(id);
      let view = branchViews.get(id);
      const isNew = !view;
      if (!view) {
        const path = document.createElementNS(namespace, 'path');
        path.dataset.cardId = id;
        view = { path, base: null, state: null };
        branchViews.set(id, view);
      }
      const { path } = view;
      const state = [...card.classList].find(name => ['running', 'waiting', 'success', 'recovered', 'failed', 'cancelled', 'unknown', 'delegating', 'intermission'].includes(name)) || 'unknown';
      const isChild = card.classList.contains('subagent-row');
      if (isChild) {
        const parent = card.closest('.session-group').querySelector('.route-card:not(.subagent-row)').getBoundingClientRect();
        const stemX = x - 10;
        path.setAttribute('d', `M${stemX} ${parent.bottom - layer.top}L${stemX} ${y - 7}C${stemX} ${y - 2} ${x - 5} ${y} ${x} ${y}`);
      } else if (tall) {
        const spine = Math.round(x) - 14;
        path.setAttribute('d', `M${spine} ${y - 12}C${spine} ${y - 4} ${x - 8} ${y} ${x} ${y}`);
      } else path.setAttribute('d',
        `M${startX} ${origin}C${startX + Math.max(18, (x - startX) * .45)} ${origin} ${startX + Math.max(18, (x - startX) * .25)} ${y} ${x} ${y}`);
      if (['running', 'waiting'].includes(state)) {
        if (!view.base) {
          view.base = document.createElementNS(namespace, 'path');
          view.base.setAttribute('class', 'branch-base');
        }
        view.base.setAttribute('class', `branch-base ${state}`);
        view.base.setAttribute('d', path.getAttribute('d'));
        place(view.base);
      } else if (view.base) {
        view.base.remove();
        view.base = null;
      }
      if (view.state !== state) {
        path.setAttribute('class', `branch ${state}`);
        view.state = state;
      }
      place(path);
      if (isNew && newIds.includes(id) && !reduced.matches && !document.hidden) {
        path.animate([{ opacity: .3, strokeWidth: 2 }, { opacity: 1, strokeWidth: 3, offset: .4 }, { opacity: .86, strokeWidth: 2 }], { duration: 320 });
      }
    });
    for (const [id, view] of branchViews) {
      if (visibleIds.has(id)) continue;
      view.base?.remove();
      view.path.remove();
      branchViews.delete(id);
    }
  }

  let motionFrame = 0;
  function followCardMotion() {
    cancelAnimationFrame(motionFrame);
    const until = performance.now() + 340;
    const tick = now => {
      drawLines();
      if (now < until && window.hudMotion.enabled()) motionFrame = requestAnimationFrame(tick);
    };
    motionFrame = requestAnimationFrame(tick);
  }

  /** 在一帧中合并布局重算，避免多次 ResizeObserver 通知造成抖动。 */
  function requestDraw(ids = []) {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => drawLines(ids));
  }

  /** 完整 ID 用于选择，显示会话名；不会把两个前缀相同的会话合并。 */
  function updateSelector(sessions) {
    sessions = sessions.filter(hasSessionContent);
    updateAgentSelector(sessions);
    sessions = filterAgent(sessions);
    const previous = selector.value;
    const options = [new Option('最近会话', '*')];
    sessions.forEach(session => options.push(new Option(`${session.label} · ${shortAccount(session.routes[0]?.account || activeSubagents(session)[0]?.account || session.completedSubagents?.[0]?.account || '账号未提供')}`, session.id)));
    selector.replaceChildren(...options);
    selector.value = sessions.some(session => session.id === previous) ? previous : '*';
  }

  /** 忽略比已渲染快照更旧的启动结果，避免订阅与首次拉取竞态。 */
  function acceptSnapshot(next, animate) {
    if (snapshot && next.revision < snapshot.revision) return;
    if (snapshot && next.revision === snapshot.revision) return;
    snapshot = next;
    updateChildPresence(next);
    updateSessionOrder(next);
    updateSelector(sessionsForView());
    render(animate);
  }

  /** IPC 错误必须可见，置顶失败不静默更换图标。 */
  function reportError(error) {
    hud.dataset.state = 'warning';
    text('watch-label', String(error));
  }

  /** 先完成事件订阅，再拉取后端已缓存快照；只在 Tauri 环境访问原生 API。 */
  async function start() {
    // 焦点仅记录交互状态，聚焦与失焦使用相同的半透明背景。
    const syncFocus = focused => { hud.dataset.focus = String(focused); };
    window.addEventListener('focus', () => syncFocus(true));
    window.addEventListener('blur', () => { syncFocus(false); hideQuotaTooltip(); });
    syncFocus(document.hasFocus());
    quotaTooltip.addEventListener('pointerenter', () => clearTimeout(quotaTooltipHideTimer));
    quotaTooltip.addEventListener('pointerleave', scheduleQuotaTooltipHide);
    document.addEventListener('keydown', event => { if (event.key === 'Escape') hideQuotaTooltip(); });
    document.addEventListener('pointerdown', event => {
      if (!quotaTooltip.contains(event.target) && !event.target.closest('.quota-item')) hideQuotaTooltip();
    });
    document.addEventListener('scroll', event => {
      if (!quotaTooltip.contains(event.target)) hideQuotaTooltip();
    }, true);
    window.addEventListener('resize', positionQuotaTooltip);
    agentSelector.addEventListener('click', () => agentOptions.hidden ? openAgentOptions() : closeAgentOptions(true));
    agentSelector.addEventListener('keydown', event => {
      if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        openAgentOptions();
        if (event.key === 'Home') agentOptions.firstElementChild?.focus();
        if (event.key === 'End') agentOptions.lastElementChild?.focus();
      }
    });
    agentOptions.addEventListener('click', event => {
      const option = event.target.closest('[data-agent]');
      if (option) chooseAgent(option.dataset.agent);
    });
    agentOptions.addEventListener('keydown', event => {
      const options = [...agentOptions.children];
      const index = options.indexOf(document.activeElement);
      if (event.key === 'Escape') { event.preventDefault(); closeAgentOptions(true); }
      else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
        event.preventDefault();
        const next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
          : Math.max(0, Math.min(options.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)));
        options[next]?.focus();
      }
    });
    document.addEventListener('pointerdown', event => {
      if (!agentSelector.contains(event.target) && !agentOptions.contains(event.target)) closeAgentOptions();
    });
    document.addEventListener('focusin', event => {
      if (!agentSelector.contains(event.target) && !agentOptions.contains(event.target)) closeAgentOptions();
    });
    selector.addEventListener('change', () => { selectedCard = null; renderedIds.clear(); quotaOrderTimer = clearTimeout(quotaOrderTimer); render(false); });
    // 两个筛选共用滚轮规则；各自累积与限速，边界停止不循环。
    const bindWheel = (control, optionsFor, indexFor, selectIndex) => {
      let deltaSum = 0;
      let wheelTime = 0;
      let wheelSwitch = -Infinity;
      control.addEventListener('wheel', event => {
        const options = optionsFor();
        if (event.ctrlKey || event.metaKey || event.altKey || event.shiftKey
            || !event.deltaY || Math.abs(event.deltaX) > Math.abs(event.deltaY)
            || control.disabled || options.length < 2) return;
        event.preventDefault();
        const now = performance.now();
        const delta = event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? control.clientHeight : 1);
        if (now - wheelTime > 180 || Math.sign(delta) !== Math.sign(deltaSum)) deltaSum = 0;
        wheelTime = now;
        if (now - wheelSwitch < 140) return;
        deltaSum += delta;
        if (Math.abs(deltaSum) < 32) return;
        const current = indexFor(options);
        const index = Math.max(0, Math.min(options.length - 1, current + Math.sign(deltaSum)));
        deltaSum = 0;
        if (index === current) return;
        wheelSwitch = now;
        selectIndex(index, options);
      }, { passive: false });
      control.addEventListener('mouseleave', () => { deltaSum = 0; wheelTime = 0; });
    };
    selector.title = '滚轮上下切换会话';
    bindWheel(selector, () => [...selector.options], () => selector.selectedIndex, index => {
      selector.selectedIndex = index;
      selector.dispatchEvent(new Event('change', { bubbles: true }));
    });
    bindWheel(agentSelector, () => [...agentOptions.children], options => options.findIndex(option => option.dataset.agent === selectedAgent),
      (index, options) => chooseAgent(options[index].dataset.agent));
    routeList.addEventListener('scroll', () => requestDraw(), { passive: true });
    byId('source-node').addEventListener('scroll', () => requestDraw(), { passive: true });
    setInterval(() => { updateCardTimes(); quotaCarousel(); updateQuotaClocks(); }, 1000);
    new ResizeObserver(() => render(false)).observe(hud);
    new ResizeObserver(() => requestDraw()).observe(document.querySelector('.routing'));
    document.querySelector('.content').addEventListener('scroll', () => requestDraw(), { passive: true });
    reduced.addEventListener('change', () => requestDraw());
    document.addEventListener('visibilitychange', () => { if (!document.hidden) render(false); });
    if (!window.__TAURI__) {
      text('watch-label', '界面预览 · 等待演示数据');
      return;
    }
    const { invoke } = window.__TAURI__.core;
    await window.__TAURI__.event.listen('route-update', event => acceptSnapshot(event.payload, true));
    await window.__TAURI__.event.listen('config-error', event => reportError(event.payload));
    const initial = await invoke('startup_state');
    acceptSnapshot(initial.snapshot, false);
    const pin = byId('pin');
    pin.setAttribute('aria-pressed', String(initial.pinned));
    pin.title = initial.pinned ? '置顶已开启' : '置顶已关闭';
    pin.addEventListener('click', async () => {
      pin.disabled = true;
      try {
        const actual = await invoke('set_pinned', { pinned: pin.getAttribute('aria-pressed') !== 'true' });
        pin.setAttribute('aria-pressed', String(actual));
        pin.title = actual ? '置顶已开启' : '置顶已关闭';
      } catch (error) { reportError(error); }
      finally { pin.disabled = false; }
    });
    document.querySelectorAll('[data-resize]').forEach(handle => {
      handle.addEventListener('pointerdown', event => {
        if (event.button !== 0) return;
        event.preventDefault();
        window.__TAURI__.webviewWindow.getCurrentWebviewWindow()
          .startResizeDragging(handle.dataset.resize).catch(reportError);
      });
    });
    byId('hide-window').addEventListener('click', () => invoke('hide_window').catch(reportError));
    byId('close-window').addEventListener('click', () => invoke('close_window').catch(reportError));
    // 右键不触发任何菜单；输入控件保留系统编辑菜单。
    document.addEventListener('contextmenu', event => { if (!event.target.closest('input,textarea,select')) event.preventDefault(); });
    byId('drag-region').addEventListener('pointerdown', event => {
      if (event.button === 0 && !event.target.closest('button,select,label')) window.__TAURI__.webviewWindow.getCurrentWebviewWindow().startDragging().catch(reportError);
    });
  }

  // 六项指标共用一个详情窗口，显式携带当前范围；详情筛选不反向修改 HUD。
  let detailsPreview = null;
  async function openDetails(metric) {
    const historical = hud.dataset.statsHistory === 'true';
    const context = {
      metric, date: byId('stats-date').dateTime || snapshot?.date,
      source: selectedAgent,
      session: historical || selector.value === '*' ? '' : selector.value,
    };
    try {
      if (window.__TAURI__) await window.__TAURI__.core.invoke('open_usage_details', { context });
      else {
        const url = `details.html?${new URLSearchParams(context)}`;
        if (detailsPreview && !detailsPreview.closed) {
          detailsPreview.dispatchEvent(new CustomEvent('hud-details-preview-context', { detail: context }));
          detailsPreview.focus();
        } else detailsPreview = window.open(url, 'hud-usage-details', 'width=960,height=720');
      }
    } catch (error) { reportError(error); }
  }
  ['tokens', 'speed', 'cost', 'cache', 'calls', 'average'].forEach(metric => {
    const element = byId(`${metric}-metric`);
    element.addEventListener('dblclick', () => openDetails(metric));
    element.addEventListener('keydown', event => {
      if (event.key === 'Enter') { event.preventDefault(); openDetails(metric); }
    });
    element.setAttribute('aria-description', '双击或按 Enter 打开用量详情');
  });

  window.addEventListener('hud-stats-view', event => {
    const { daily, scope, date, costError, historical } = event.detail;
    renderStats(daily, scope, costError, historical);
    window.dispatchEvent(new CustomEvent('hud-hourly-update', { detail: {
      hourly: daily.hourly, date, scope, error: costError,
    } }));
  });

  // 独立浏览器验证可通过已声明事件注入脱敏 fixture；产品从不自行生成演示数据。
  window.addEventListener('hud-preview-snapshot', event => { if (!window.__TAURI__) acceptSnapshot(event.detail, true); });
  start().catch(reportError);
})();
