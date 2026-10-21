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
  let animations = [];
  let frame = 0;
  const quotaViews = new Map();
  let quotaRenderKey = '';
  let quotaOrderTimer = 0;

  /** 文本节点保留完整 title，显示受 CSS 单行省略约束。 */
  function text(id, value) {
    const element = byId(id);
    element.textContent = value;
    element.title = value;
  }

  /** 大数仅用于可读展示，精确值由统计块的 title 提供。 */
  function compact(value) {
    if (value >= 1e9) return `${(value / 1e9).toFixed(2)}B`;
    if (value >= 1e6) return `${(value / 1e6).toFixed(2)}M`;
    if (value >= 1e3) return `${(value / 1e3).toFixed(1)}k`;
    return Number(value).toLocaleString('zh-CN');
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
    if (/gpt|codex|o[134](?:-|$)/.test(name)) return 'openai';
    if (/claude|sonnet|opus|haiku|fable/.test(name)) return 'anthropic';
    if (name.includes('grok')) return 'grok';
    if (name.includes('gemini')) return 'gemini';
    if (name.includes('deepseek')) return 'deepseek';
    if (/glm|zhipu/.test(name)) return 'zhipu';
    return null;
  }

  /** 展示统一为完成、运行、异常三类；异常类别按取消、失败、未知区分。 */
  function statusInfo(card) {
    switch (card.state) {
      case 'running':
      case 'waiting': return ['', '运行中', 'running'];
      case 'success':
      case 'recovered': return ['√', '已完成', 'success'];
      case 'cancelled': return ['x', '已取消', 'error'];
      case 'failed': return ['x', '失败', 'error'];
      default: return ['x', '状态未知', 'error'];
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
    const family = modelIcon(card.model);
    if (family) {
      const image = document.createElement('img');
      image.src = `icons/${family}.svg`;
      image.alt = family;
      icon.append(image);
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
    if (status === 'running') {
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
    top.append(account, tag);
    const bottom = document.createElement('span');
    bottom.className = 'card-bottom';
    const label = document.createElement('span');
    label.className = 'card-account';
    label.textContent = shortAccount(card.account);
    const separator = document.createElement('span');
    separator.className = 'divider';
    separator.textContent = '·';
    const model = document.createElement('span');
    model.className = 'card-model';
    model.textContent = card.model;
    const effort = document.createElement('span');
    effort.className = 'effort';
    effort.textContent = card.effort;
    const intensity = card.effort.toLowerCase();
    if (['low', 'medium', 'high', 'xhigh', 'max', 'ultra'].includes(intensity)) effort.dataset.level = intensity;
    bottom.append(model, separator, label, effort);
    copy.append(top, bottom);
    const metrics = document.createElement('span');
    metrics.className = 'card-metrics';
    const tokens = document.createElement('span');
    tokens.className = 'card-tokens';
    tokens.textContent = card.tokens == null ? '待统计 Token' : `${compact(card.tokens)} Token`;
    const speed = document.createElement('span');
    speed.className = 'card-tps';
    speed.textContent = card.tokensPerSecond == null ? '— TPS' : `${card.tokensPerSecond.toFixed(1)} TPS`;
    metrics.title = '本次请求 Token = 输入 + 输出 + 缓存读取；TPS = 输出 / (总耗时 − 首字耗时)。进行中尚无精确用量时等待最终记录。';
    metrics.append(tokens, speed);
    button.append(icon, copy, metrics);
    button.addEventListener('click', () => {
      selectedCard = card.id;
      routeList.querySelectorAll('.route-card').forEach(element => element.classList.toggle('selected', element === button));
      renderNotes([card.note, session.notes[1]]);
    });
    return button;
  }

  /** 下方两个固定槽同步更新，不追加历史行。 */
  function renderNotes(notes) {
    notes.forEach((note, index) => {
      text(`line${index + 1}-time`, note.time);
      text(`line${index + 1}-kind`, note.kind);
      text(`line${index + 1}-text`, note.text);
    });
  }

  /** 当日输出速度为 Σout / Σ(ms−ttft_ms)，不把输入或缓存读计为输出吞吐。 */
  function renderStats(stats, scope) {
    text('stats-date', snapshot.date.slice(5));
    const short = innerHeight <= 420;
    const tokens = short ? new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 0 }).format(stats.tokens) : compact(stats.tokens);
    text('daily-tokens', tokens);
    byId('hud').dataset.costStale = String(Boolean(snapshot.costError));
    text('request-count', `${stats.requests.toLocaleString('zh-CN')} 次请求`);
    byId('tokens-metric').title = `当日 Token ${stats.tokens.toLocaleString('zh-CN')}；输入 + 输出 + 缓存读取，不含 kind=routing 的内部分类调用`;
    text('cost-status', snapshot.costError ? '旧读数' : '');
    text('daily-cost', stats.cost == null ? '未定价' : `$${stats.cost.toFixed(short && stats.cost >= 100 ? 0 : 2)}`);
    text('cost-unit', stats.cost == null ? '' : 'USD');
    text('cost-coverage', stats.cost == null ? (stats.unpricedRequests ? `${stats.unpricedRequests} 条未定价` : '等待 Magpie 费用') : `${stats.pricedRequests} 条已定价${stats.unpricedRequests ? ` · ${stats.unpricedRequests} 条未定价` : ''}${snapshot.costError ? ' · 旧读数' : ''}`);
    byId('cost-metric').title = `${stats.cost == null ? '未定价' : `$${stats.cost.toFixed(2)} USD`}\n${snapshot.costError || 'Magpie 按模型价格计算的当日美元等价费用，沿用 Magpie 用量页口径（含内部路由）；订阅账号的金额是使用量估值，未定价请求不计入。'}`;
    text('daily-speed', stats.tokensPerSecond === null ? '—' : stats.tokensPerSecond.toFixed(1));
    byId('speed-metric').title = `最近 10 个会话加权输出速度 = Σ输出 Token / Σ(总耗时 − 首字耗时)；${stats.speedSamples} 条有效记录，不包含等待首字的时间。选中单个会话时只显示该会话。`;
    text('stats-scope', scope);
  }

  /** 来源按真实 Agent 合并，最多两个时与 Magpie 纵向排列，给会话留出宽度。 */
  function renderSources(sessions) {
    const groups = new Map();
    sessions.forEach(item => {
      if (!groups.has(item.agent)) groups.set(item.agent, new Set());
      groups.get(item.agent).add(item.request);
    });
    const list = byId('source-list');
    list.replaceChildren();
    for (const [agent, requests] of groups) {
      const entry = document.createElement('div');
      entry.className = 'source-entry';
      const heading = document.createElement('div');
      heading.className = 'source-heading';
      const icon = document.createElement('span');
      icon.className = 'source-icon';
      const family = /claude/i.test(agent) ? 'claude' : modelIcon(agent);
      if (family) {
        const image = document.createElement('img');
        image.src = `icons/${family}.svg`;
        image.alt = '';
        icon.append(image);
      } else icon.textContent = '↗';
      const name = document.createElement('strong');
      name.textContent = agent;
      heading.append(icon, name);
      const request = document.createElement('span');
      request.className = 'source-request';
      request.textContent = [...requests].join(' · ');
      request.title = request.textContent;
      entry.append(heading, request);
      list.append(entry);
    }
    if (!groups.size) list.textContent = '等待请求';
    hud.dataset.sources = groups.size <= 2 ? 'compact' : 'multiple';
  }

  /** 按当前会话选择决定额度展示项；同一分钟内保持上次排序，只更新数值。 */
  function renderQuotas(sessions, forceOrder) {
    const list = byId('quota-list');
    const scroll = list.scrollTop;
    const accounts = new Map();
    sessions.flatMap(session => session.routes.slice(0, 4)).forEach(card => {
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
      const windows = quota.windows.length ? quota.windows : [null];
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
    text('quota-freshness', quotaFailed ? '旧读数' : '');
    byId('quota-freshness').title = snapshot.quotaError || (readings.length ? `最近读数 ${new Date(Math.min(...readings)).toLocaleString('zh-CN')}` : '暂无额度读数');
    list.scrollTop = scroll;
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
    const session = snapshot.sessions.find(item => item.id === selector.value);
    const sessions = session ? [session] : snapshot.sessions.filter(item => item.routes.some(card => ['running', 'waiting'].includes(card.state)));
    if (!session) sessions.push(...snapshot.sessions.filter(item => !sessions.includes(item)).slice(0, Math.max(0, 3 - sessions.length)));
    const oldScroll = routeList.scrollTop;
    hud.dataset.state = snapshot.state;
    text('watch-label', snapshot.watchLabel);
    byId('watch-label').title = snapshot.error || snapshot.watchLabel;
    text('router-host', snapshot.magpieHost);
    renderSources(sessions);
    if (hud.dataset.sources === 'multiple' && innerWidth < 980) hud.dataset.layout = 'tall';
    text('session-count', session ? session.label : `${snapshot.sessions.length} 个会话 · 显示 ${sessions.length} 个`);
    byId('source-node').title = sessions.map(item => `${item.agent}\n${item.request}`).join('\n');
    renderStats(session ? session.daily : snapshot.daily, session ? session.label : '全部会话');
    renderQuotas(sessions, forceQuotaOrder);
    const fragment = document.createDocumentFragment();
    const newIds = [];
    const currentIds = new Set();
    sessions.forEach(item => item.routes.slice(0, 4).forEach(card => {
      fragment.append(createCard(card, item));
      currentIds.add(card.id);
      if (!renderedIds.has(card.id)) newIds.push(card.id);
    }));
    routeList.replaceChildren(fragment);
    routeList.hidden = sessions.length === 0;
    routeList.scrollTop = oldScroll;
    byId('empty-route').hidden = sessions.length > 0;
    byId('inbound-active').style.opacity = sessions.length ? '1' : '0';
    const picked = sessions.flatMap(item => item.routes).find(card => card.id === selectedCard);
    if (picked) renderNotes([picked.note, sessions.find(item => item.id === picked.sessionId).notes[1]]);
    else { selectedCard = null; renderNotes(session ? session.notes : snapshot.notes); }
    renderedIds = currentIds;
    requestDraw(animate ? newIds : []);
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
    animations.forEach(animation => animation.cancel());
    animations = [];
    lineLayer.replaceChildren();
    lineLayer.setAttribute('viewBox', `0 0 ${layer.width} ${layer.height}`);
    routeList.querySelectorAll('.route-card').forEach(card => {
      const rectangle = card.getBoundingClientRect();
      const x = rectangle.left - layer.left;
      const y = rectangle.top + rectangle.height / 2 - layer.top;
      const path = document.createElementNS(namespace, 'path');
      const state = [...card.classList].find(name => ['running', 'waiting', 'failed', 'cancelled', 'unknown'].includes(name)) || (card.dataset.current === 'true' ? '' : 'inactive');
      path.setAttribute('d', tall
        ? `M${startX} ${origin}V${bendY}H${trunkX}V${Math.max(bendY, y - 16)}Q${trunkX} ${y} ${x} ${y}`
        : `M${startX} ${origin}C${startX + Math.max(18, (x - startX) * .45)} ${origin} ${startX + Math.max(18, (x - startX) * .25)} ${y} ${x} ${y}`);
      if (state === 'running') {
        const base = document.createElementNS(namespace, 'path');
        base.setAttribute('d', path.getAttribute('d'));
        base.setAttribute('class', 'branch-base');
        lineLayer.append(base);
      }
      path.setAttribute('class', `branch ${state}`);
      path.style.animationDelay = `${-(performance.now() % 1000)}ms`;
      lineLayer.append(path);
      if (newIds.includes(card.dataset.cardId) && !reduced.matches && !document.hidden) {
        animations.push(path.animate([{ opacity: .3, strokeWidth: 2 }, { opacity: 1, strokeWidth: 3, offset: .4 }, { opacity: .86, strokeWidth: 2 }], { duration: 320 }));
      }
    });
  }

  /** 在一帧中合并布局重算，避免多次 ResizeObserver 通知造成抖动。 */
  function requestDraw(ids = []) {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => drawLines(ids));
  }

  /** 完整 ID 用于选择，显示会话名；不会把两个前缀相同的会话合并。 */
  function updateSelector(sessions) {
    const previous = selector.value;
    const options = [new Option('最近会话', '*')];
    sessions.forEach(session => options.push(new Option(`${session.label} · ${shortAccount(session.routes[0]?.account || '账号未提供')}`, session.id)));
    selector.replaceChildren(...options);
    selector.value = sessions.some(session => session.id === previous) ? previous : '*';
  }

  /** 忽略比已渲染快照更旧的启动结果，避免订阅与首次拉取竞态。 */
  function acceptSnapshot(next, animate) {
    if (snapshot && next.revision < snapshot.revision) return;
    if (snapshot && next.revision === snapshot.revision) return;
    snapshot = next;
    updateSelector(next.sessions);
    render(animate);
  }

  /** IPC 错误必须可见，置顶失败不静默更换图标。 */
  function reportError(error) {
    hud.dataset.state = 'warning';
    text('watch-label', String(error));
  }

  /** 先完成事件订阅，再拉取后端已缓存快照；只在 Tauri 环境访问原生 API。 */
  async function start() {
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
    byId('hide-window').addEventListener('click', () => window.__TAURI__.webviewWindow.getCurrentWebviewWindow().hide().catch(reportError));
    byId('drag-region').addEventListener('pointerdown', event => {
      if (event.button === 0 && !event.target.closest('button,select,label')) window.__TAURI__.webviewWindow.getCurrentWebviewWindow().startDragging().catch(reportError);
    });
  }

  // 独立浏览器验证可通过已声明事件注入脱敏 fixture；产品从不自行生成演示数据。
  window.addEventListener('hud-preview-snapshot', event => { if (!window.__TAURI__) acceptSnapshot(event.detail, true); });
  start().catch(reportError);
})();
