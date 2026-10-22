'use strict';

// 仅渲染结构化小时汇总；图表不读取日志或向网关发请求。
(() => {
  const svg = document.getElementById('hourly-chart');
  const plot = document.getElementById('hourly-plot');
  const tooltip = document.getElementById('hourly-tooltip');
  const ns = 'http://www.w3.org/2000/svg';
  const number = new Intl.NumberFormat('en', { notation: 'compact', maximumSignificantDigits: 3 });
  let series = [];
  let activeHour = null;
  let key = '';
  let frame = 0;
  let context = '';
  let geometry = null;
  const measure = document.createElement('canvas').getContext('2d');

  function element(tag, attrs, text) {
    const node = document.createElementNS(ns, tag);
    for (const [name, value] of Object.entries(attrs)) node.setAttribute(name, value);
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function money(value) {
    if (!Number.isFinite(value)) return '—';
    if (value > 0 && value < .01) return '<$0.01';
    return `$${value.toFixed(value < 10 ? 2 : value < 100 ? 1 : 0)}`;
  }

  /** 各单位独立缩放；最值外留 5%，常数和全零数据也保留有限范围。 */
  function domain(values) {
    if (!values.length) return [0, 1];
    const min = Math.min(...values);
    const max = Math.max(...values);
    if (max === 0) return [0, 1];
    return [min * .95, max * 1.05];
  }

  function readout(hour) {
    const item = series[hour];
    const interval = `${String(hour).padStart(2, '0')}:00–${String(hour + 1).padStart(2, '0')}:00`;
    return `${interval}\n${item.tokens == null ? '—' : item.tokens.toLocaleString('zh-CN')} Token\n${item.cost == null ? '金额未提供' : `$${item.cost.toFixed(6)} USD${item.partial ? ' · 部分观测' : ''}`}`;
  }

  function inspect(hour) {
    if (!series.length || !geometry) return;
    activeHour = Math.max(0, Math.min(23, hour));
    tooltip.textContent = readout(activeHour);
    tooltip.hidden = false;
    const { width, left, slot } = geometry;
    tooltip.style.left = `${Math.max(0, Math.min(width - Math.min(180, width), left + slot * activeHour - 40))}px`;
    tooltip.style.maxWidth = `${Math.max(120, width)}px`;
    const cursor = svg.querySelector('.hourly-cursor');
    cursor.setAttribute('x1', left + slot * (activeHour + .5));
    cursor.setAttribute('x2', left + slot * (activeHour + .5));
    cursor.removeAttribute('hidden');
    plot.setAttribute('aria-label', `${context}，${readout(activeHour).replaceAll('\n', '，')}，左右方向键切换小时`);
  }

  function hide() {
    activeHour = null;
    tooltip.hidden = true;
    svg.querySelector('.hourly-cursor')?.setAttribute('hidden', '');
  }

  function draw() {
    frame = 0;
    const width = Math.max(100, plot.clientWidth);
    const height = plot.clientHeight;
    const left = 3;
    const span = width - 6;
    const slot = span / 24;
    const compact = getComputedStyle(plot).getPropertyValue('--hourly-compact').trim() === '1';
    const top = compact ? 11 : 20;
    const bottom = height - (compact ? 17 : 23);
    geometry = { width, left, slot };
    svg.setAttribute('viewBox', `0 0 ${width} ${height}`);
    const fragment = document.createDocumentFragment();
    const tokenValues = series.map(item => item.tokens).filter(Number.isFinite);
    const costValues = series.map(item => item.cost).filter(Number.isFinite);
    const tokenMinMax = domain(tokenValues);
    const costMinMax = domain(costValues);
    const [tokenMin, tokenMax] = tokenMinMax;
    const [costMin, costMax] = costMinMax;
    svg.dataset.tokenDomain = tokenMinMax.join(',');
    svg.dataset.costDomain = costMinMax.join(',');
    const y = (value, min, max) => bottom - (value - min) / (max - min) * (bottom - top);
    const x = hour => left + (hour + .5) * slot;
    const barWidth = Math.max(1, Math.min(10, slot * .62));
    for (const hour of [4, 8, 12, 16, 20]) fragment.append(element('line', {
      class: 'hourly-grid', x1: left + span * hour / 24, x2: left + span * hour / 24, y1: top, y2: bottom,
    }));
    const peaks = [
      ...(tokenValues.length ? [{ value: Math.max(...tokenValues), min: tokenMin, max: tokenMax }] : []),
      ...(costValues.length ? [{ value: Math.max(...costValues), min: costMin, max: costMax }] : []),
    ];
    if (peaks.length) {
      const peakY = Math.min(...peaks.map(peak => y(peak.value, peak.min, peak.max)));
      fragment.append(element('line', { class: 'hourly-reference', x1: left, x2: left + span, y1: peakY, y2: peakY }));
    }
    series.forEach((item, hour) => {
      const end = Number.isFinite(item.tokens) ? y(item.tokens, tokenMin, tokenMax) : bottom;
      const bar = element('rect', { class: 'hourly-bar', x: x(hour) - barWidth / 2, y: end,
        width: barWidth, height: Math.max(0, bottom - end), rx: Math.min(1.5, barWidth / 3), 'data-hour': hour });
      fragment.append(bar);
    });
    let path = '';
    let connected = false;
    series.forEach((item, hour) => {
      if (!Number.isFinite(item.cost)) { connected = false; return; }
      path += `${connected ? 'L' : 'M'}${x(hour)} ${y(item.cost, costMin, costMax)} `;
      connected = true;
    });
    fragment.append(element('path', { class: 'hourly-cost', d: path.trim() }));
    const labels = [];
    const labelTop = compact ? 10 : !costValues.length || series.some(item => item.partial) ? 25 : 11;
    const labelBottom = height - (compact ? 19 : 26);
    const labelGap = 13;
    measure.font = `${compact ? 8 : 9}px ${getComputedStyle(plot).fontFamily}`;
    const extremes = (field, min, max, format) => {
      const valid = series.map((item, hour) => ({ hour, value: item[field] })).filter(item => Number.isFinite(item.value));
      if (!valid.length) return;
      const low = valid.reduce((a, b) => b.value < a.value ? b : a);
      const high = valid.reduce((a, b) => b.value > a.value ? b : a);
      for (const [point, kind] of [[high, 'max'], [low, 'min']]) {
        if (kind === 'min' && high.value === low.value) continue;
        const px = x(point.hour);
        const py = y(point.value, min, max);
        if (field === 'cost') fragment.append(element('circle', { class: 'hourly-dot', cx: px, cy: py, r: 2.5 }));
        const value = format(point.value);
        const labelWidth = measure.measureText(value).width + 3;
        const lx = Math.max(labelWidth / 2 + 1, Math.min(width - labelWidth / 2 - 1, px));
        const preferredY = Math.max(labelTop, Math.min(labelBottom, py + (field === 'cost' ? 13 : -6)));
        // 最多四个极值标签；小图同时横向避让，引线保留数值与原始点的归属。
        const rows = [preferredY, ...Array.from({ length: Math.floor((labelBottom - labelTop) / labelGap) + 1 }, (_, index) => labelTop + index * labelGap)];
        const columns = compact ? [lx, ...Array.from({ length: Math.floor((width - 2) / (labelWidth + 4)) }, (_, index) => 1 + labelWidth / 2 + index * (labelWidth + 4))] : [lx];
        const candidates = columns.flatMap(x => rows.map(y => ({ x, y })))
          .sort((a, b) => Math.hypot(a.x - lx, a.y - preferredY) - Math.hypot(b.x - lx, b.y - preferredY));
        const position = candidates.find(candidate => !labels.some(other =>
          Math.abs(other.x - candidate.x) < (other.width + labelWidth) / 2 + 3 && Math.abs(other.y - candidate.y) < labelGap)) ?? { x: lx, y: preferredY };
        if (Math.abs(position.x - px) > 8 || Math.abs(position.y - py) > 17) fragment.append(element('path', {
          d: `M${px} ${py}L${position.x} ${position.y + 2}`, stroke: 'var(--muted)', 'stroke-width': .6, fill: 'none', opacity: .6,
        }));
        fragment.append(element('text', { class: 'hourly-extreme', x: position.x, y: position.y, 'text-anchor': 'middle', 'data-series': field, 'data-extreme': kind }, value));
        labels.push({ ...position, width: labelWidth });
      }
    };
    extremes('tokens', tokenMin, tokenMax, value => number.format(value));
    extremes('cost', costMin, costMax, money);
    if (!compact) {
      if (!costValues.length) fragment.append(element('text', { class: 'hourly-note', x: width - 2, y: 10, 'text-anchor': 'end' }, '金额待更新'));
      else if (series.some(item => item.partial)) fragment.append(element('text', { class: 'hourly-note', x: width - 2, y: 10, 'text-anchor': 'end' }, '部分观测'));
    }
    for (const hour of [0, 8, 16, 24]) fragment.append(element('text', {
      class: 'hourly-tick', x: left + span * hour / 24, y: height - 3,
      'text-anchor': hour === 0 ? 'start' : hour === 24 ? 'end' : 'middle',
    }, `${hour}h`));
    fragment.append(element('line', { class: 'hourly-cursor', x1: 0, x2: 0, y1: top - 3, y2: bottom, hidden: '' }));
    svg.replaceChildren(fragment);
    if (activeHour != null) inspect(activeHour);
  }

  const schedule = () => { if (!frame) frame = setTimeout(draw, 0); };
  new ResizeObserver(schedule).observe(plot);
  plot.addEventListener('pointermove', event => {
    if (!geometry) return;
    inspect(Math.floor((event.clientX - plot.getBoundingClientRect().left - geometry.left) / geometry.slot));
  });
  plot.addEventListener('pointerleave', () => { if (document.activeElement !== plot) hide(); });
  plot.addEventListener('focus', () => inspect(activeHour ?? new Date().getHours()));
  plot.addEventListener('blur', hide);
  plot.addEventListener('keydown', event => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End', 'Escape'].includes(event.key)) return;
    event.preventDefault();
    if (event.key === 'Escape') { hide(); return; }
    inspect(event.key === 'Home' ? 0 : event.key === 'End' ? 23 : (activeHour ?? 0) + (event.key === 'ArrowRight' ? 1 : -1));
  });
  window.addEventListener('hud-hourly-update', event => {
    const { hourly, date, scope, error } = event.detail;
    const nextKey = JSON.stringify([hourly, date, scope, error, new Date().getHours()]);
    if (nextKey === key) return;
    key = nextKey;
    const today = date === new Date().toLocaleDateString('en-CA');
    series = Array.from({ length: 24 }, (_, hour) => {
      const value = hourly?.[hour];
      return {
        tokens: today && hour > new Date().getHours() ? null : Number.isFinite(value?.tokens) ? value.tokens : null,
        cost: today && hour > new Date().getHours() ? null : Number.isFinite(value?.cost) ? value.cost : null,
        partial: Boolean(value?.costPartial),
      };
    });
    context = `${date} ${scope}，24 小时 Token 柱状图与金额折线图，分别缩放，范围为最值外留百分之五；全局金额使用网关小时历史，会话金额记录连续观测增量，缺失时段留空${error ? '，金额为上次读数' : ''}`;
    plot.title = context;
    plot.setAttribute('aria-label', `${context}，左右方向键查看每小时数值`);
    const rows = series.map((item, hour) => {
      const row = document.createElement('tr');
      for (const value of [`${hour}h–${hour + 1}h`, item.tokens == null ? '—' : item.tokens.toLocaleString('zh-CN'), item.cost == null ? '—' : `${item.cost.toFixed(6)}${item.partial ? '（部分观测）' : ''}`]) {
        const cell = document.createElement('td');
        cell.textContent = value;
        row.append(cell);
      }
      return row;
    });
    document.querySelector('#hourly-table tbody').replaceChildren(...rows);
    schedule();
  });
})();
