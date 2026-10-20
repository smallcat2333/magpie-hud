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

  /** 状态文字与符号一起展示，失败/取消不依赖颜色区分。 */
  function statusInfo(card) {
    switch (card.state) {
      case 'recovered': return ['✓', card.detail.startsWith('重试') ? '重试成功' : '已恢复'];
      case 'failed': return ['!', '失败'];
      case 'cancelled': return ['−', '已取消'];
      case 'unknown': return ['?', '状态未知'];
      default: return ['✓', '已完成'];
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
    const [symbol, state] = statusInfo(card);
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
    account.className = 'account';
    account.textContent = shortAccount(card.account);
    const tag = document.createElement('span');
    tag.className = 'state-tag';
    const mark = document.createElement('span');
    mark.className = 'state-symbol';
    mark.textContent = symbol;
    tag.append(mark, document.createTextNode(state));
    top.append(account, tag);
    const bottom = document.createElement('span');
    bottom.className = 'card-bottom';
    const label = document.createElement('span');
    label.className = 'session-short';
    label.textContent = session.label;
    const separator = document.createElement('span');
    separator.className = 'divider';
    separator.textContent = '·';
    const model = document.createElement('span');
    model.className = 'card-model';
    model.textContent = card.model;
    const effort = document.createElement('span');
    effort.className = 'effort';
    effort.textContent = card.effort;
    bottom.append(label, separator, model, effort);
    copy.append(top, bottom);
    button.append(icon, copy);
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
    text('daily-tokens', compact(stats.tokens));
    text('request-count', `${stats.requests.toLocaleString('zh-CN')} 次请求`);
    byId('tokens-metric').title = `当日 Token ${stats.tokens.toLocaleString('zh-CN')}；输入 + 输出 + 缓存读取，不含 kind=routing 的内部分类调用`;
    text('daily-cost', stats.credits === null ? '未记录' : stats.credits.toFixed(2));
    text('cost-unit', stats.credits === null ? '' : 'Credits');
    text('cost-coverage', stats.creditedRequests ? `${stats.creditedRequests}/${stats.requests} 条有费用` : '日志无金额 / Credits');
    byId('cost-metric').title = '只累计日志 credits，单位为 Credits，不是美元；缺少定价或金额时不能推算真实账单。仅覆盖带 credits 的记录。';
    text('daily-speed', stats.tokensPerSecond === null ? '—' : stats.tokensPerSecond.toFixed(1));
    byId('speed-metric').title = `当日加权输出速度 = Σ输出 Token / Σ(总耗时 − 首字耗时)；${stats.speedSamples} 条有效记录，不包含等待首字的时间。`;
    text('stats-scope', scope);
  }

  /** 根据当前会话选择生成分支和两行说明，保留滚动位置。 */
  function render(animate) {
    if (!snapshot) return;
    const session = snapshot.sessions.find(item => item.id === selector.value);
    const sessions = session ? [session] : snapshot.sessions.slice(0, 3);
    const oldScroll = routeList.scrollTop;
    hud.dataset.state = snapshot.state;
    text('watch-label', snapshot.watchLabel);
    byId('watch-label').title = snapshot.error || snapshot.watchLabel;
    text('router-host', snapshot.magpieHost);
    text('agent-name', sessions[0]?.agent || '等待请求');
    text('agent-request', sessions[0]?.request || '请求模型待记录');
    text('session-count', session ? session.label : `${snapshot.sessions.length} 个会话 · 最近 ${sessions.length} 个`);
    byId('source-node').title = sessions[0] ? `${sessions[0].agent}\n${sessions[0].request}` : '尚无今日记录';
    renderStats(session ? session.daily : snapshot.daily, session ? session.label : '全部会话');
    const fragment = document.createDocumentFragment();
    const newIds = [];
    const currentIds = new Set();
    sessions.forEach(item => item.routes.slice(0, 4).forEach(card => {
      fragment.append(createCard(card, item));
      currentIds.add(card.id);
      if (!renderedIds.has(card.id)) newIds.push(card.id);
    }));
    routeList.replaceChildren(fragment);
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
    const area = byId('route-area').getBoundingClientRect();
    const router = document.querySelector('.router-emblem').getBoundingClientRect();
    const origin = router.top + router.height / 2 - area.top;
    animations.forEach(animation => animation.cancel());
    animations = [];
    lineLayer.replaceChildren();
    lineLayer.setAttribute('viewBox', `0 0 ${area.width} ${area.height}`);
    routeList.querySelectorAll('.route-card').forEach(card => {
      const rectangle = card.getBoundingClientRect();
      const x = rectangle.left - area.left;
      const y = rectangle.top + rectangle.height / 2 - area.top;
      const path = document.createElementNS(namespace, 'path');
      const state = [...card.classList].find(name => ['failed', 'cancelled', 'unknown'].includes(name)) || (card.dataset.current === 'true' ? '' : 'inactive');
      path.setAttribute('d', `M2 ${origin}C${Math.max(14, x * .55)} ${origin} ${Math.max(14, x * .35)} ${y} ${x - 2} ${y}`);
      path.setAttribute('class', `branch ${state}`);
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

  /** 完整 ID 用于选择，显示短号；不会把两个前缀相同的会话合并。 */
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
    selector.addEventListener('change', () => { selectedCard = null; renderedIds.clear(); render(false); });
    routeList.addEventListener('scroll', () => requestDraw(), { passive: true });
    new ResizeObserver(() => requestDraw()).observe(byId('route-area'));
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
    byId('hide-window').addEventListener('click', () => window.__TAURI__.webviewWindow.getCurrentWebviewWindow().hide().catch(reportError));
    byId('drag-region').addEventListener('pointerdown', event => {
      if (event.button === 0 && !event.target.closest('button,select,label')) window.__TAURI__.webviewWindow.getCurrentWebviewWindow().startDragging().catch(reportError);
    });
  }

  // 独立浏览器验证可通过已声明事件注入脱敏 fixture；产品从不自行生成演示数据。
  window.addEventListener('hud-preview-snapshot', event => { if (!window.__TAURI__) acceptSnapshot(event.detail, true); });
  start().catch(reportError);
})();
