'use strict';

// 仅消费 Rust 结构化快照。账号与日志一律写入 textContent，不把外部文本当 HTML。
(() => {
  const byId = id => document.getElementById(id);
  const hud = byId('hud');
  const selector = byId('session-select');
  const routeList = byId('routes');
  const lineLayer = byId('route-lines');
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  const namespace = 'http://www.w3.org/2000/svg';
  let snapshot = null;
  let selectedCard = null;
  let renderedIds = new Set();
  let frame = 0;
  const branchViews = new Map();
  const quotaViews = new Map();
  const subagentNumbers = new Map();
  const childPresence = new Map();
  const childSessions = new Map();
  let childPresenceDate = '';
  const childGraceMs = 10_000;
  let childGraceTimer = 0;
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
    const family = selecting ? null : modelIcon(card.model);
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
    model.textContent = /^(正在选择账号|正在选择模型|模型待定)$/.test(card.model) ? '正在选路' : card.model;
    model.title = card.model;
    const effort = document.createElement('span');
    effort.className = 'effort';
    effort.textContent = /^(强度待确定|强度未提供|待定)$/.test(card.effort) ? '待定' : card.effort;
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
    button.append(icon, copy, metrics);
    button.addEventListener('click', () => {
      selectedCard = card.id;
      routeList.querySelectorAll('.route-card,.subagent-row').forEach(element => element.classList.toggle('selected', element === button));
      renderNotes([card.note, session.notes[1]]);
    });
    return button;
  }

  /** 只挂载有活跃模型请求的子 Agent，编号在所属会话内保持稳定。 */
  function activeSubagents(session) {
    return (session.subagents || []).filter(card => ['running', 'waiting'].includes(card.state));
  }

  /** 活跃消失时开始十秒保留，重复快照不续期，新请求沿用同一子项位置。 */
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
      const detail = '模型请求暂时结束，等待下一次请求或最终用量；空闲满 10 秒后收起';
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
    button.replaceChildren(button.querySelector('.model-icon'), name, model, state, effort, metrics);
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

  /** 下方两个固定槽同步更新，不追加历史行。 */
  function renderNotes(notes) {
    notes.forEach((note, index) => {
      text(`line${index + 1}-time`, note.time);
      text(`line${index + 1}-kind`, note.kind);
      text(`line${index + 1}-text`, note.text);
    });
  }

  /** 六项数值和可聚焦区域共用完整说明，避免数字节点自己的 title 遮住详情。 */
  function metric(id, valueId, value, detail) {
    text(valueId, value);
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
  function renderStats(stats, scope) {
    text('stats-date', snapshot.date.slice(5));
    const tokens = compact(stats.tokens);
    text('request-count', `${stats.requests} 次请求`);
    metric('tokens-metric', 'daily-tokens', tokens, `Token · ${scope}\n当日 ${stats.tokens.toLocaleString('zh-CN')} Token；输入 + 输出 + 缓存读取，不含内部分类调用。`);
    text('cost-status', snapshot.costError ? '旧读数' : '');
    text('cost-unit', stats.cost == null ? '' : 'USD');
    text('cost-coverage', stats.cost == null ? (stats.unpricedRequests ? `${stats.unpricedRequests} 条未定价` : '等待 Magpie 费用') : `${stats.pricedRequests} 条已定价${stats.unpricedRequests ? ` · ${stats.unpricedRequests} 条未定价` : ''}${snapshot.costError ? ' · 旧读数' : ''}`);
    metric('cost-metric', 'daily-cost', callCost(stats.cost),
      `费用估值 · ${scope}\n${stats.cost == null ? '未定价' : `$${stats.cost.toFixed(6)} USD`}\n${snapshot.costError || 'Magpie 当日美元等价费用，含内部分类调用；订阅账号为用量估值。'}\n${stats.pricedRequests} 次已定价，${stats.unpricedRequests} 次未定价。`);
    metric('speed-metric', 'daily-speed', Number.isFinite(stats.tokensPerSecond) ? compactFormat.format(stats.tokensPerSecond).replace('K', 'k') : '—',
      `TPS · ${scope}\n${Number.isFinite(stats.tokensPerSecond) ? `${stats.tokensPerSecond.toFixed(3)} tok/s\n` : ''}${sessionSpeedScope(scope)}加权输出速度 = Σ输出 Token / Σ(总耗时 − 首字耗时)；${stats.speedSamples} 条有效记录，不包含等待首字的时间。`);
    const cache = stats.cacheHitRate;
    metric('cache-metric', 'daily-cache', Number.isFinite(cache) ? `${(cache * 100).toFixed(1)}%` : '—',
      `缓存命中率 · ${scope}\n${Number.isFinite(cache) ? `${(cache * 100).toFixed(3)}%` : '暂无有效缓存输入'}\n缓存读取 Token / (输入 Token + 缓存读取 Token)，按完成请求加权；不含输出与内部分类调用。`);
    metric('calls-metric', 'daily-calls', String(stats.requests),
      `调用次数 · ${scope}\n当日 ${stats.requests} 次已结束调用，含主会话和子 Agent，不含内部分类调用。`);
    const average = stats.costPerCall;
    metric('average-metric', 'daily-average', callCost(average), `单次调用费用 · ${scope}\n${Number.isFinite(average)
      ? `$${average.toFixed(8)} USD / 次 = Magpie 费用 / ${stats.pricedRequests} 次已定价调用（含内部分类），不计未定价调用。${snapshot.costError ? '\n费用为旧读数' : ''}`
      : '暂无可用的已定价调用，等待 Magpie 费用读数。'}`);
    text('stats-scope', scope);
  }

  function sessionSpeedScope(scope) {
    return scope === '全部会话' ? '最近 10 个会话' : '当前会话';
  }

  /** 来源按真实 Agent 合并，最多两个时与 Magpie 纵向排列，给会话留出宽度。 */
  function renderSources(sessions) {
    const agents = new Set(sessions.map(item => item.agent));
    const list = byId('source-list');
    list.replaceChildren();
    for (const agent of agents) {
      const entry = document.createElement('div');
      entry.className = 'source-entry';
      const heading = document.createElement('div');
      heading.className = 'source-heading';
      const icon = document.createElement('span');
      icon.className = 'source-icon';
      const family = /claude[\s-]*code/i.test(agent) ? 'claudecode-color' : /claude/i.test(agent) ? 'claude-color' : modelIcon(agent);
      if (family) {
        const image = document.createElement('img');
        image.src = `icons/${family}.svg`;
        image.alt = '';
        icon.append(image);
      } else icon.textContent = '↗';
      const name = document.createElement('strong');
      name.textContent = agent;
      name.title = agent;
      heading.append(icon, name);
      entry.append(heading);
      list.append(entry);
    }
    if (!agents.size) list.textContent = '等待请求';
    hud.dataset.sources = agents.size <= 2 ? 'compact' : 'multiple';
  }

  /** 按当前会话选择决定额度展示项；“最近会话”汇总当天全部会话。 */
  function renderQuotas(sessions, forceOrder) {
    const list = byId('quota-list');
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
        (quota.user ? quota.user === card.account : quota.kind === 'balance' && quota.name === card.account));
      if (candidates.length !== 1) continue;
      const quota = candidates[0];
      const read = quota.readAt || quota.asOf;
      if (read && !Number.isNaN(Date.parse(read))) readings.push(new Date(read));
      const shortest = quota.windows.length ? quota.windows.reduce((best, span) => windowRank(span) < windowRank(best) ? span : best) : null;
      const windows = [shortest];
      windows.forEach((span, index) => {
        const windowName = span?.name === '5 hours' ? '5h' : ['7 days', 'Weekly'].includes(span?.name) ? '周' : span?.name || '额度';
        entries.push({ key, index, card, models, quota, span, windowName });
      });
    }
    const orderKey = entries.map(entry => `${entry.key}|${entry.index}`).join('\n');
    if (!forceOrder && quotaRenderKey && orderKey !== quotaRenderKey) {
      const ranks = new Map(quotaRenderKey.split('\n').map((key, index) => [key, index]));
      entries.sort((a, b) => (ranks.get(`${a.key}|${a.index}`) ?? Infinity) - (ranks.get(`${b.key}|${b.index}`) ?? Infinity));
    } else quotaRenderKey = orderKey;
    const next = new Map(entries.map(entry => [`${entry.key}|${entry.index}`, entry]));
    for (const [key, view] of quotaViews) {
      if (!next.has(key)) { view.item.remove(); quotaViews.delete(key); }
    }
    let previous = null;
    for (const entry of entries) {
      const itemKey = `${entry.key}|${entry.index}`;
      const view = quotaViews.get(itemKey) || createQuotaItem(entry);
      quotaViews.set(itemKey, view);
      updateQuotaItem(view, entry);
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
    const quotaFailed = snapshot.quotaError || [...quotaViews.values()].some(view => view.item.dataset.stale === 'true');
    list.title = [quotaFailed ? '旧读数' : '', snapshot.quotaError || (readings.length ? `最近读数 ${new Date(Math.min(...readings)).toLocaleString('zh-CN')}` : '暂无额度读数')].filter(Boolean).join('\n');
    list.scrollTop = scroll;
  }

  /** 同一账号只展示最短窗口；无法识别时排在已知窗口之后。 */
  function windowRank(span) {
    const name = (span?.name || '').trim().toLowerCase();
    if (/^(weekly|week|周|每周)$/.test(name)) return 7 * 1440;
    if (/^(daily|day|日|每天)$/.test(name)) return 1440;
    if (/^(monthly|month|月|每月)$/.test(name)) return 30 * 1440;
    const match = name.match(/(\d+(?:\.\d+)?)\s*(minutes?|mins?|m|分钟|hours?|hrs?|h|小时|days?|d|天|weeks?|w|周|months?|月)\b/i)
      || name.match(/(\d+(?:\.\d+)?)\s*(分钟|小时|天|周|月)/);
    if (!match) return Infinity;
    const unit = match[2];
    const scale = /^(months?|月)$/.test(unit) ? 43200 : /^(weeks?|w|周)$/.test(unit) ? 10080
      : /^(days?|d|天)$/.test(unit) ? 1440 : /^(hours?|hrs?|h|小时)$/.test(unit) ? 60 : 1;
    return Number(match[1]) * scale;
  }

  /** 只创建稳定元素，不在每次快照中重排。 */
  function createQuotaItem() {
    const item = document.createElement('div');
    item.className = 'quota-item';
    item.tabIndex = 0;
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
    track.append(fill);
    const value = document.createElement('strong');
    row.append(label, track, value);
    item.append(header, row);
    return { item, account, reset, label, track, fill, value };
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

  /** 更新同一额度项的数值和提示，不改变其在列表中的位置。 */
  function updateQuotaItem(view, { card, models, quota, span, windowName }) {
    view.item.dataset.stale = String(Boolean(snapshot.quotaError || quota.error));
    view.account.textContent = shortAccount(card.account);
    view.account.title = card.account;
    const resetTime = span?.resetsAt && !Number.isNaN(Date.parse(span.resetsAt)) ? new Date(span.resetsAt) : null;
    view.reset.textContent = resetRemaining(resetTime);
    view.reset.dateTime = resetTime ? span.resetsAt : '';
    view.reset.title = resetTime ? `${resetTime.toLocaleString('zh-CN', { hour12: false })} 重置` : '重置时间未提供';
    view.label.textContent = windowName;
    const used = span && !span.unlimited ? (Number.isFinite(span.used) ? span.used : Number.isFinite(span.remaining) ? 100 - span.remaining : null) : null;
    const percent = used === null ? null : Math.min(100, Math.max(0, used));
    const summary = percent !== null ? `已用 ${percent.toFixed(0)}%` : span?.unlimited ? '不限量' : quota.balance || '未提供';
    view.value.textContent = percent !== null ? `${percent.toFixed(0)}%` : span?.unlimited ? '∞' : '—';
    view.track.setAttribute('aria-label', `${card.account} ${windowName} 已用额度`);
    view.track.setAttribute('aria-valuetext', summary);
    view.track.dataset.unknown = String(percent === null);
    if (percent !== null) {
      view.track.setAttribute('aria-valuemin', '0');
      view.track.setAttribute('aria-valuemax', '100');
      view.track.setAttribute('aria-valuenow', String(percent));
      view.fill.style.width = `${percent}%`;
    } else view.fill.style.width = '0';
    view.track.parentElement.dataset.low = String(percent !== null && percent >= 90);
    const read = quota.readAt || quota.asOf;
    view.item.title = [card.account, [...models].join(' · '), [span?.family || span?.pool, windowName, summary].filter(Boolean).join(' · '), view.reset.title, read ? `读数 ${read}` : '', snapshot.quotaError || quota.error || ''].filter(Boolean).join('\n');
    view.item.setAttribute('aria-label', view.item.title.replaceAll('\n', '，'));
  }

  /** 根据当前会话选择生成分支和两行说明，保留滚动位置。 */
  function render(animate) {
    const forceQuotaOrder = !quotaOrderTimer;
    if (!quotaOrderTimer) quotaOrderTimer = setTimeout(() => { quotaOrderTimer = 0; render(false); }, 60_000);
    hud.dataset.layout = innerWidth / innerHeight < 1.25 || innerWidth < 680 ? 'tall' : 'wide';
    if (!snapshot) return;
    const allSessions = sessionsForView().filter(hasSessionContent);
    const visibleSessions = allSessions.filter(item => item.routes.length || subagentsForView(item).length);
    const session = allSessions.find(item => item.id === selector.value);
    const active = visibleSessions.filter(item => item.routes.some(card => ['running', 'waiting'].includes(card.state)) || activeSubagents(item).length);
    const recent = visibleSessions.filter(item => !active.includes(item));
    const chooseSessions = () => session ? [session] : [...active, ...recent.slice(0, Math.max(0, visibleSessionCapacity() - active.length))];
    let sessions = chooseSessions();
    const oldScroll = routeList.scrollTop;
    hud.dataset.state = snapshot.state;
    text('watch-label', snapshot.watchLabel);
    byId('watch-label').title = snapshot.error || snapshot.watchLabel;
    text('router-host', snapshot.magpieHost);
    renderSources(sessions);
    if (hud.dataset.sources === 'multiple' && innerWidth < 980) hud.dataset.layout = 'tall';
    renderStats(session ? session.daily : snapshot.daily, session ? session.label : '全部会话');
    window.dispatchEvent(new CustomEvent('hud-hourly-update', { detail: {
      hourly: (session ? session.daily : snapshot.daily).hourly,
      date: snapshot.date, scope: session ? session.label : '全部会话', error: snapshot.costError,
    } }));
    renderQuotas(session ? [session] : snapshot.sessions, forceQuotaOrder);
    if (hud.dataset.layout === 'tall') {
      sessions = chooseSessions();
      renderSources(sessions);
    }
    text('session-count', session ? session.label : `${visibleSessions.length}会话, 显示${sessions.length}个`);
    byId('source-node').title = [...new Set(sessions.map(item => item.agent))].join('\n');
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
      parents.forEach(card => { group.append(createCard(card, item)); rememberCard(card); });
      if (children.length) {
        group.classList.add('has-subagents');
        const list = document.createElement('div');
        list.className = 'subagent-list';
        list.setAttribute('role', 'group');
        list.setAttribute('aria-label', `${item.label}的${session ? '子 Agent 请求' : '活跃子 Agent'}`);
        children.forEach(card => { list.append(createSubagent(card, item)); rememberCard(card); });
        group.append(list);
      }
      fragment.append(group);
    });
    routeList.replaceChildren(fragment);
    routeList.hidden = sessions.length === 0;
    routeList.scrollTop = oldScroll;
    byId('empty-route').hidden = sessions.length > 0;
    const inbound = byId('inbound-active');
    const hasRequests = sessions.some(item => item.routes.some(card => ['running', 'waiting'].includes(card.state)) || activeSubagents(item).length);
    inbound.classList.toggle('flowing', hasRequests);
    inbound.style.opacity = sessions.length ? '1' : '0';
    const picked = sessions.flatMap(item => [...item.routes, ...subagentsForView(item, Boolean(session))]).find(card => card.id === selectedCard);
    if (picked) renderNotes([picked.note, sessions.find(item => item.id === picked.sessionId).notes[1]]);
    else { selectedCard = null; renderNotes(session ? session.notes : snapshot.notes); }
    renderedIds = currentIds;
    requestDraw(animate ? newIds : []);
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

  /** 用实际路由块位置画贝塞尔分支，列表滚动和窗口大小变化后重新对齐。 */
  function drawLines(newIds = []) {
    const layer = lineLayer.getBoundingClientRect();
    const router = document.querySelector('.router-emblem').getBoundingClientRect();
    const area = byId('route-area').getBoundingClientRect();
    const tall = hud.dataset.layout === 'tall';
    const startX = (tall ? router.left + router.width / 2 : router.right) - layer.left;
    const origin = (tall ? router.bottom : router.top + router.height / 2) - layer.top;
    const trunkX = area.left - layer.left + 12;
    const bendY = Math.max(origin + 6, area.top - layer.top - 6);
    const visibleIds = new Set();
    lineLayer.setAttribute('viewBox', `0 0 ${layer.width} ${layer.height}`);
    let previousNode = null;
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
        path.setAttribute('d', `M${stemX} ${parent.bottom - layer.top}V${y - 5}Q${stemX} ${y} ${stemX + 5} ${y}H${x}`);
      } else path.setAttribute('d', tall
        ? `M${startX} ${origin}V${bendY}H${trunkX}V${Math.max(bendY, y - 16)}Q${trunkX} ${y} ${x} ${y}`
        : `M${startX} ${origin}C${startX + Math.max(18, (x - startX) * .45)} ${origin} ${startX + Math.max(18, (x - startX) * .25)} ${y} ${x} ${y}`);
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

  /** 在一帧中合并布局重算，避免多次 ResizeObserver 通知造成抖动。 */
  function requestDraw(ids = []) {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => drawLines(ids));
  }

  /** 完整 ID 用于选择，显示会话名；不会把两个前缀相同的会话合并。 */
  function updateSelector(sessions) {
    sessions = sessions.filter(hasSessionContent);
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
    window.addEventListener('blur', () => syncFocus(false));
    syncFocus(document.hasFocus());
    selector.addEventListener('change', () => { selectedCard = null; renderedIds.clear(); quotaOrderTimer = clearTimeout(quotaOrderTimer); render(false); });
    routeList.addEventListener('scroll', () => requestDraw(), { passive: true });
    new ResizeObserver(() => render(false)).observe(hud);
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
    const menu = byId('window-menu');
    const showMenu = event => {
      event.preventDefault();
      menu.hidden = false;
      const bounds = menu.getBoundingClientRect();
      menu.style.left = `${Math.max(8, Math.min(event.clientX, innerWidth - bounds.width - 8))}px`;
      menu.style.top = `${Math.max(8, Math.min(event.clientY, innerHeight - bounds.height - 8))}px`;
    };
    document.addEventListener('contextmenu', event => { if (!event.target.closest('input,textarea,select')) showMenu(event); });
    document.addEventListener('pointerdown', event => { if (!menu.contains(event.target)) menu.hidden = true; });
    menu.addEventListener('click', async event => {
      const action = event.target.closest('[data-window-action]')?.dataset.windowAction;
      menu.hidden = true;
      if (action === 'hide') await invoke('hide_window').catch(reportError);
      if (action === 'reset') await invoke('reset_window_position').catch(reportError);
    });
    byId('drag-region').addEventListener('pointerdown', event => {
      if (event.button === 0 && !event.target.closest('button,select,label')) window.__TAURI__.webviewWindow.getCurrentWebviewWindow().startDragging().catch(reportError);
    });
  }

  // 独立浏览器验证可通过已声明事件注入脱敏 fixture；产品从不自行生成演示数据。
  window.addEventListener('hud-preview-snapshot', event => { if (!window.__TAURI__) acceptSnapshot(event.detail, true); });
  start().catch(reportError);
})();
