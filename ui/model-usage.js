'use strict';

// 今日账本的模型分布；费用仅在选中该口径时按需查询。
(() => {
  const section = document.getElementById('model-usage');
  const sidebar = document.querySelector('.sidebar');
  const quota = document.querySelector('.quota');
  const plot = document.getElementById('model-usage-plot');
  const summary = document.getElementById('model-usage-summary');
  const table = document.getElementById('model-usage-table-body');
  const tooltip = document.getElementById('model-usage-tooltip');
  const ns = 'http://www.w3.org/2000/svg';
  // 三种互相可区分的主色，其余以中性纹理与直接文字区分，不以颜色承载身份。
  const colors = ['#3987e5', '#d95926', '#199e70', '#776d86', '#716d63'];
  const effortColors = { low: '#9ccfff', medium: '#8dd7b0', high: '#e6cf89', xhigh: '#f2b17c', max: '#ef97ab', ultra: '#e4acdf', none: '#969aa5' };
  const effortOrder = ['none', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra'];
  const format = new Intl.NumberFormat('zh-CN');
  const compact = value => value >= 1e6 ? `${(value / 1e6).toFixed(1)}M` : value >= 1e3 ? `${(value / 1e3).toFixed(1)}k` : format.format(value);
  let rows = [], scope = '', mode = 'tokens', selected = null, hideTimer = 0, layoutFrame = 0, lastRender = '', colorsByModel = new Map();
  let context = { date: '', session: null, sourceKey: null, revision: 0 };
  let costState = { key: '', status: 'idle', hasData: false, rows: [], warnings: [] }, costRequest = 0;
  const node = (tag, className, text) => {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text !== undefined) element.textContent = text;
    return element;
  };
  // 模型和强度的 TPS 均用有效输出量 / 生成时长；面积用各自 TPS 的相对比例。
  const speed = item => item.generationMs > 0 ? item.outputTokens * 1000 / item.generationMs : null;
  const measure = item => {
    const value = mode === 'cost' ? item.cost : mode === 'speed'
      ? item.members?.length ? item.members.reduce((sum, member) => sum + (speed(member) ?? 0), 0) : speed(item)
      : item.tokens;
    return Number.isFinite(value) ? Math.max(0, value) : 0;
  };
  const number = value => Number.isFinite(value) ? format.format(value) : '—';
  const dollars = value => Number.isFinite(value) ? `$${value.toFixed(value > 0 && value < .01 ? 4 : 2)}` : '—';
  const speedText = item => speed(item) === null ? 'TPS 未记录' : `${speed(item).toFixed(1)} TPS`;
  const metricText = item => mode === 'cost' ? dollars(item.priced ? item.cost : null) : mode === 'speed' ? speedText(item) : `${compact(item.tokens)} Token`;
  const percent = (part, total) => total > 0 ? `${(part * 100 / total).toFixed(1)}%` : '—';
  const effortLabel = effort => {
    const value = (effort || '').trim();
    if (!value || value === '强度未提供') return '未记录';
    const canonical = value.toLowerCase();
    return effortOrder.includes(canonical) ? canonical === 'none' ? 'None' : canonical[0].toUpperCase() + canonical.slice(1) : value;
  };
  const makeEmpty = name => ({ name, requests: 0, tokens: 0, outputTokens: 0, generationMs: 0, speedSamples: 0,
    unknownTokenRequests: 0, cost: 0, priced: 0, unpriced: 0, unmatched: 0, outcomes: {}, efforts: new Map(), members: [] });
  const addAmounts = (target, row) => {
    for (const key of ['requests', 'tokens', 'outputTokens', 'generationMs', 'speedSamples', 'unknownTokenRequests', 'priced', 'unpriced', 'unmatched']) {
      target[key] += Math.max(0, Number(row[key]) || 0);
    }
    if (Number.isFinite(row.cost) && row.cost >= 0) target.cost += row.cost;
  };
  const getEffort = (model, name) => {
    if (!model.efforts.has(name)) model.efforts.set(name, makeEmpty(name));
    return model.efforts.get(name);
  };
  function collect(input) {
    const models = new Map();
    for (const row of input) {
      const name = (row.model || '').trim() || '服务模型未提供';
      if (!models.has(name)) models.set(name, makeEmpty(name));
      const model = models.get(name);
      addAmounts(model, row);
      const outcome = row.outcome || 'unknown';
      model.outcomes[outcome] = (model.outcomes[outcome] || 0) + Math.max(0, Number(row.requests) || 0);
      addAmounts(getEffort(model, effortLabel(row.effort)), row);
    }
    if (mode === 'cost' && costState.hasData) {
      // 后端已按会话别名和来源精确筛选，再按模型与强度合计。
      for (const row of costState.rows) {
        const name = (row.model || '').trim() || '服务模型未提供';
        // 费用与账本分开合计，未匹配的数据保留未知覆盖量。
        if (!models.has(name)) models.set(name, makeEmpty(name));
        const model = models.get(name);
        const expense = { cost: row.cost, priced: row.priced, unpriced: row.unpriced, unmatched: row.unmatched };
        addAmounts(model, expense);
        addAmounts(getEffort(model, effortLabel(row.effort)), expense);
      }
    }
    return [...models.values()];
  }
  function sumModels(items) {
    const result = makeEmpty('其他');
    // 已合并的组继续展平，悬停时展示每个真实模型及其强度。
    result.members = items.flatMap(item => item.members.length ? item.members : [item]);
    for (const item of result.members) {
      addAmounts(result, item);
      for (const [key, value] of Object.entries(item.outcomes)) result.outcomes[key] = (result.outcomes[key] || 0) + value;
      for (const [name, effort] of item.efforts) addAmounts(getEffort(result, name), effort);
    }
    return result;
  }
  function colorFor(name) {
    if (name === '其他') return '#626b7a';
    if (!colorsByModel.has(name)) colorsByModel.set(name, colors[colorsByModel.size] || '#696f79');
    return colorsByModel.get(name);
  }
  // 按面积二分；优先保留每个能单独展示的模型，其余汇入同一个“其他”。
  function rectangles(items, x, y, width, height) {
    if (!items.length || width <= 0 || height <= 0) return [];
    if (items.length === 1) return [{ item: items[0], x, y, width, height }];
    const total = items.reduce((sum, item) => sum + measure(item), 0);
    let split = 1, cumulative = measure(items[0]), best = Math.abs(cumulative - total / 2);
    for (let index = 2; index < items.length; index++) {
      cumulative += measure(items[index - 1]);
      const distance = Math.abs(cumulative - total / 2);
      if (distance < best) { best = distance; split = index; }
    }
    const first = items.slice(0, split), second = items.slice(split);
    const ratio = first.reduce((sum, item) => sum + measure(item), 0) / total;
    // 最后一条过窄时沿另一方向分割，仍保留精确面积。
    let horizontal = width >= height;
    const span = horizontal ? width : height;
    const cross = horizontal ? height : width;
    if (items.length === 2 && Math.min(span * ratio, span * (1 - ratio)) < 20
        && Math.min(cross * ratio, cross * (1 - ratio)) >= 20) horizontal = !horizontal;
    if (horizontal) {
      const side = width * ratio;
      return [...rectangles(first, x, y, side, height), ...rectangles(second, x + side, y, width - side, height)];
    }
    const side = height * ratio;
    return [...rectangles(first, x, y, width, side), ...rectangles(second, x, y + side, width, height - side)];
  }
  function layoutModels(models, width, height) {
    const ordered = models.filter(item => measure(item) > 0)
      .sort((a, b) => measure(b) - measure(a) || a.name.localeCompare(b.name));
    let visible = ordered.slice(0, 5), combined = ordered.slice(5);
    for (;;) {
      const displayed = combined.length ? [...visible, sumModels(combined)] : [...visible];
      const rects = rectangles(displayed, 0, 0, width, height);
      const tiny = rects.filter(rect => rect.width < 20 || rect.height < 20);
      if (!tiny.length) return rects;
      let smallest = tiny.filter(rect => rect.item.name !== '其他')
        .sort((a, b) => measure(a.item) - measure(b.item))[0];
      if (!smallest && visible.length > 1) {
        const candidate = visible[visible.length - 1];
        smallest = rects.find(rect => rect.item === candidate);
      }
      // 最后一个小组可能不足 20px，保留真实面积并提供键盘访问。
      if (!smallest || visible.length <= 1) return rects;
      visible = visible.filter(item => item !== smallest.item);
      combined.push(smallest.item);
    }
  }
  function updateSpace() {
    layoutFrame = 0;
    const forceShow = innerHeight > 900;
    sidebar.dataset.modelUsageReserved = String(forceShow);
    if (!forceShow && document.getElementById('hud').dataset.layout !== 'wide') {
      section.hidden = true;
      closeTooltip();
      return;
    }
    const style = getComputedStyle(sidebar);
    const gap = parseFloat(style.rowGap) || 0;
    const available = sidebar.getBoundingClientRect().bottom - quota.getBoundingClientRect().bottom - gap - (parseFloat(style.paddingBottom) || 0);
    // 高于 900px 必须显示；较矮窗口仍按剩余空间判断。
    const show = forceShow || (section.hidden ? available >= 180 : available >= 170);
    if (section.hidden === show) {
      section.hidden = !show;
      if (!show) closeTooltip();
      else if (mode === 'cost') loadCosts();
      else render(true);
    }
  }
  function scheduleSpace() {
    updateSpace();
    if (!layoutFrame) layoutFrame = requestAnimationFrame(updateSpace);
  }
  function closeTooltip() {
    clearTimeout(hideTimer);
    selected = null;
    tooltip.hidden = true;
    plot.querySelectorAll('[aria-describedby]').forEach(cell => cell.removeAttribute('aria-describedby'));
  }
  function scheduleClose() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (!tooltip.matches(':hover') && !plot.querySelector('.model-usage-cell:hover, .model-usage-cell:focus')) closeTooltip();
    }, 220);
  }
  function positionTooltip(cell) {
    if (!cell?.isConnected || tooltip.hidden) return;
    const anchor = cell.getBoundingClientRect(), width = tooltip.offsetWidth, height = tooltip.offsetHeight;
    let left = anchor.right + 12;
    if (left + width > innerWidth - 8) left = anchor.left - width - 12;
    left = Math.max(8, Math.min(innerWidth - width - 8, left));
    const top = Math.max(8, Math.min(innerHeight - height - 8, anchor.top - 4));
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${top}px`;
  }
  const costIsStale = () => costState.hasData && (costState.status !== 'ready' || costState.key !== JSON.stringify(context));
  function renderTooltip(model, total) {
    const value = measure(model);
    const title = node('h3', '', model.name);
    const suffix = mode === 'speed' ? ` · TPS 相对占比 ${percent(value, total)}` : ` · 占当前范围 ${percent(value, total)}`;
    const desc = node('p', 'model-usage-tooltip-meta', `今日 ${metricText(model)}${suffix}`);
    const content = [title, desc];
    if (model.unknownTokenRequests && mode === 'tokens') content.push(node('p', 'model-usage-tooltip-meta', '部分请求未记录输入或输出 Token，占比按已知用量统计'));
    if (mode === 'cost') {
      if (costIsStale()) content.push(node('p', 'model-usage-tooltip-meta', costState.status === 'error' ? '费用更新失败，暂显示上次结果' : '费用正在更新，暂显示上次结果'));
      content.push(node('p', 'model-usage-tooltip-meta', `已定价 ${number(model.priced)} · 未定价 ${number(model.unpriced)} · 未匹配 ${number(model.unmatched)}；只计算已知费用`));
    }
    if (model.members.length) {
      const heading = node('p', 'model-usage-tooltip-meta', model.members.length > 1 ? `合并 ${model.members.length} 个模型；其中过小的份额已并入` : '面积过小，已合并显示');
      const list = node('ul', 'model-usage-tooltip-list');
      for (const item of [...model.members].sort((a, b) => measure(b) - measure(a))) {
        const li = node('li'); li.append(node('span', '', item.name), node('span', '', `${metricText(item)} · ${percent(measure(item), value)}`)); list.append(li);
      }
      content.push(heading, list);
    }
    {
      const value = measure(model);
      const items = [...model.efforts.values()].sort((a, b) => {
        const x = effortOrder.indexOf(a.name.toLowerCase()), y = effortOrder.indexOf(b.name.toLowerCase());
        return (x < 0 ? 99 : x) - (y < 0 ? 99 : y) || a.name.localeCompare(b.name);
      });
      const breakdown = node('div', 'model-usage-breakdown');
      const wrap = node('div', 'model-usage-donut-wrap');
      const svg = document.createElementNS(ns, 'svg'); svg.classList.add('model-usage-donut'); svg.setAttribute('viewBox', '0 0 84 84'); svg.setAttribute('aria-hidden', 'true');
      const background = document.createElementNS(ns, 'circle'); background.setAttribute('cx', '42'); background.setAttribute('cy', '42'); background.setAttribute('r', '31'); background.setAttribute('fill', 'none'); background.setAttribute('stroke', '#525862'); background.setAttribute('stroke-width', '10'); svg.append(background);
      const legend = node('div', 'model-usage-legend');
      // 强度速度的占比以各强度 TPS 之和为分母；模型整体 TPS 仍按输出/时长加权。
      const effortTotal = items.reduce((sum, item) => sum + measure(item), 0);
      let offset = 0;
      const circumference = 2 * Math.PI * 31;
      for (const item of items) {
        const amount = measure(item), fraction = effortTotal > 0 ? amount / effortTotal : 0;
        const color = effortColors[item.name.toLowerCase()] || '#9297a3';
        if (fraction > 0) {
          const circle = document.createElementNS(ns, 'circle');
          for (const [key, val] of Object.entries({ cx: '42', cy: '42', r: '31', fill: 'none', stroke: color, 'stroke-width': '10', 'stroke-dasharray': `${fraction * circumference} ${circumference}`, 'stroke-dashoffset': `${-offset * circumference}` })) circle.setAttribute(key, val);
          svg.append(circle); offset += fraction;
        }
        const line = node('div', 'model-usage-legend-row');
        const swatch = node('i'); swatch.style.backgroundColor = color;
        line.append(swatch, node('span', 'model-usage-legend-name', item.name), node('span', 'model-usage-legend-value', `${metricText(item)} · ${percent(amount, effortTotal)}`)); legend.append(line);
      }
      const center = node('div', 'model-usage-center'); center.append(node('strong', '', mode === 'cost' ? dollars(model.priced ? model.cost : null) : mode === 'speed' ? (speed(model) === null ? '—' : speed(model).toFixed(1)) : compact(value)), node('span', '', mode === 'cost' ? '费用' : mode === 'speed' ? 'TPS' : 'Token'));
      wrap.append(svg, center); breakdown.append(wrap, legend);
      const explanation = mode === 'speed' ? '强度 TPS 相对占比 · 按各强度 TPS 之和计算' : mode === 'cost' ? '强度占比 · 按模型已知费用计算' : '强度占比 · 以此模型的 Token 为分母';
      content.push(node('p', 'model-usage-tooltip-meta', explanation), breakdown);
    }
    if (mode === 'speed') content.push(node('p', 'model-usage-outcomes', model.name === '其他'
      ? '模型汇总 TPS = 有效输出 Token ÷ 生成时长；面积按各模型 TPS 之和计算。'
      : 'TPS = 有效成功请求的输出 Token ÷ 生成时长；面积按各模型 TPS 的相对大小计算。'));
    tooltip.replaceChildren(...content);
  }
  function openTooltip(cell, model, total) {
    clearTimeout(hideTimer);
    plot.querySelectorAll('[aria-describedby]').forEach(other => other.removeAttribute('aria-describedby'));
    selected = model.name;
    renderTooltip(model, total);
    tooltip.hidden = false;
    cell.setAttribute('aria-describedby', tooltip.id);
    positionTooltip(cell);
  }
  function render(force = false) {
    const models = collect(rows);
    const total = models.reduce((sum, item) => sum + measure(item), 0);
    const key = JSON.stringify([mode, scope, rows, costState]);
    if (!force && key === lastRender) return;
    lastRender = key;
    table.replaceChildren(...models.map(item => {
      const tr = node('tr');
      for (const text of [item.name, number(item.tokens), mode === 'cost' ? dollars(item.priced ? item.cost : null) : '未加载', speed(item) === null ? '—' : `${speed(item).toFixed(1)} TPS`]) tr.append(node('td', '', text));
      return tr;
    }));
    const unknownTokens = models.some(item => item.unknownTokenRequests > 0);
    summary.title = mode === 'speed' ? '矩形面积按模型 TPS 的相对大小；各模型 TPS 按有效输出和生成时长加权' : mode === 'cost' ? (costState.warnings || []).join('；') : unknownTokens ? '部分请求未记录 Token，仅展示已知用量' : '';
    summary.textContent = mode === 'cost' ? costState.status === 'loading' && !costState.hasData ? '正在匹配费用…' : costState.status === 'error' && !costState.hasData ? '费用读取失败，点击费用重试' : `已知费用 ${dollars(models.some(item => item.priced) ? total : null)}${models.some(item => item.unmatched || item.unpriced) ? ' · 部分未定价' : ''}`
      : mode === 'speed' ? '今日 · 矩形按 TPS 相对大小' : `今日 · ${compact(total)} Token${unknownTokens ? ' · 部分未知' : ''}`;
    if (section.hidden) return;
    if (mode === 'cost' && !costState.hasData) {
      if (costState.status === 'loading') {
        plot.replaceChildren(node('div', 'model-usage-empty', '正在读取费用…'));
        closeTooltip(); return;
      }
      if (costState.status === 'error') {
        plot.replaceChildren(node('div', 'model-usage-empty', '费用读取失败，点击费用重试'));
        closeTooltip(); return;
      }
      if (costState.status !== 'ready') {
        plot.replaceChildren(node('div', 'model-usage-empty', '请选择费用'));
        closeTooltip(); return;
      }
    }
    const sorted = models.filter(item => measure(item) > 0).sort((a, b) => measure(b) - measure(a) || a.name.localeCompare(b.name));
    if (!sorted.length) {
      const message = mode === 'cost' ? models.some(item => item.priced) ? '已知费用为 $0.00' : '暂无可用的已定价费用'
        : mode === 'speed' ? models.length ? '暂无可用的 TPS' : '暂无已完成请求'
        : models.length ? '暂无已记录 Token' : '暂无已完成请求';
      plot.replaceChildren(node('div', 'model-usage-empty', message));
      closeTooltip(); return;
    }
    // 清理上一次空/加载状态，再按当前尺寸计算比例。
    plot.querySelector('.model-usage-empty')?.remove();
    sorted.forEach(item => colorFor(item.name));
    const rects = layoutModels(sorted, plot.clientWidth, plot.clientHeight);
    const existing = new Map([...plot.querySelectorAll('.model-usage-cell')].map(cell => [cell.dataset.model, cell]));
    for (const { item, x, y, width, height } of rects) {
      let cell = existing.get(item.name);
      if (!cell) {
        cell = node('button', 'model-usage-cell'); cell.type = 'button'; cell.dataset.model = item.name;
        cell.append(node('strong'), node('span'));
        cell.addEventListener('pointerenter', () => openTooltip(cell, cell._model, cell._total));
        cell.addEventListener('focus', () => openTooltip(cell, cell._model, cell._total));
        cell.addEventListener('pointerleave', scheduleClose); cell.addEventListener('blur', scheduleClose);
        plot.append(cell);
      }
      existing.delete(item.name); cell._model = item; cell._total = total;
      Object.assign(cell.style, { left: `${x + 1}px`, top: `${y + 1}px`, width: `${Math.max(0, width - 2)}px`, height: `${Math.max(0, height - 2)}px`, backgroundColor: colorFor(item.name) });
      cell.dataset.small = String(width < 35 || height < 24);
      cell.firstElementChild.textContent = item.name;
      cell.lastElementChild.textContent = mode === 'tokens' ? `${compact(item.tokens)} ${percent(measure(item), total)}`
        : mode === 'cost' ? `${dollars(item.priced ? item.cost : null).replace(/^\$/, '')} ${percent(measure(item), total)}`
        : speedText(item);
      cell.setAttribute('aria-label', `${item.name}，${metricText(item)}，${mode === 'speed' ? 'TPS 相对占比' : '占比'} ${percent(measure(item), total)}`);
    }
    existing.forEach(cell => cell.remove());
    if (selected) {
      const current = rects.find(rect => rect.item.name === selected)?.item;
      const cell = [...plot.children].find(item => item.dataset.model === selected);
      if (current && cell) { renderTooltip(current, total); positionTooltip(cell); }
      else closeTooltip();
    }
  }
  async function loadCosts() {
    const key = JSON.stringify(context);
    if (section.hidden || mode !== 'cost' || !window.__TAURI__?.core) { render(true); return; }
    if (costState.key === key && (costState.status === 'ready' || costState.status === 'loading')) { render(); return; }
    const request = ++costRequest;
    // 同一范围保留上次结果，首次查询仍显示读取状态。
    costState = { ...costState, key, status: 'loading' };
    render(true);
    try {
      const result = await window.__TAURI__.core.invoke('query_model_costs', { session: context.session, sourceKey: context.sourceKey });
      if (request !== costRequest || key !== JSON.stringify(context)) return;
      if (result.date !== context.date || result.ledgerRevision !== context.revision) {
        throw new Error('账本已更新，请重试');
      }
      if (result.fetchFailed) throw new Error('费用读取失败，请重试');
      // 网关可能尚未收录账本：全部无法匹配时保留此前已定价结果，允许重试。
      if (costState.hasData && costState.rows.some(row => row.priced > 0)
          && (!Array.isArray(result.rows) || !result.rows.length
            || result.rows.every(row => !row.priced && !row.unpriced))) {
        throw new Error('费用尚未匹配，请重试');
      }
      costState = { key, status: 'ready', hasData: true, rows: result.rows || [], warnings: result.warnings || [] };
    } catch (error) {
      if (request !== costRequest || key !== JSON.stringify(context)) return;
      costState = { ...costState, key, status: 'error', warnings: [String(error)] };
    }
    render(true);
  }
  section.querySelectorAll('[data-model-mode]').forEach(button => button.addEventListener('click', () => {
    mode = button.dataset.modelMode;
    section.querySelectorAll('[data-model-mode]').forEach(other => other.setAttribute('aria-pressed', String(other === button)));
    if (mode === 'cost') loadCosts();
    else render(true);
  }));
  tooltip.addEventListener('pointerenter', () => clearTimeout(hideTimer));
  tooltip.addEventListener('pointerleave', scheduleClose);
  window.addEventListener('resize', () => { scheduleSpace(); render(true); });
  window.addEventListener('blur', closeTooltip);
  document.addEventListener('keydown', event => { if (event.key === 'Escape') closeTooltip(); });
  const observer = new ResizeObserver(scheduleSpace); observer.observe(sidebar); observer.observe(quota);
  const plotObserver = new ResizeObserver(() => render(true)); plotObserver.observe(plot);
  window.hudModelUsage = {
    update(data, currentScope, nextContext) {
      rows = Array.isArray(data) ? data : [];
      scope = currentScope || '全部会话';
      const previous = context;
      context = { date: '', session: null, sourceKey: null, revision: 0, ...nextContext };
      const scopeChanged = previous.date !== context.date || previous.session !== context.session || previous.sourceKey !== context.sourceKey;
      if (scopeChanged) {
        costRequest++;
        costState = { key: '', status: 'idle', hasData: false, rows: [], warnings: [] };
      } else if (previous.revision !== context.revision) {
        // 同一范围账本追加时保留旧数据，但丢弃旧请求，后台重新匹配费用。
        costRequest++;
        costState = { ...costState, key: '', status: 'idle' };
      }
      scheduleSpace();
      if (mode === 'cost') loadCosts();
      else render();
    },
  };
})();
