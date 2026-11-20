'use strict';

// 两条共用时间轴的图分别按 TPS 和会话数缩放。
(() => {
  const root = document.getElementById('active-trend');
  const plot = root.querySelector('.active-trend-plot');
  const canvas = plot.querySelector('canvas');
  const context = canvas.getContext('2d');
  const [peakLabel, floorLabel, meanLabel] = plot.querySelectorAll('.active-trend-value');
  const readout = root.querySelector('output');
  const time = new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
  const padding = 5;
  const slots = 30;
  const windowMs = 60_000;
  let points = [];
  let selectedEnd = null;

  function guide(x1, x2, y1, y2) {
    context.beginPath();
    context.moveTo(x1, y1);
    context.lineTo(x2, y2);
    context.stroke();
  }

  function paint() {
    const { width, height } = canvas.getBoundingClientRect();
    if (!width || !height) return;
    const scale = devicePixelRatio || 1;
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
    context.setTransform(scale, 0, 0, scale, 0, 0);

    const step = (width - 2 * padding) / slots;
    const x = index => padding + (slots - points.length + index + .5) * step;
    const barBase = height - 2;
    const barHeight = 20;
    const lineBase = barBase - barHeight - 17;
    const yMax = Math.max(1, ...points.map(point => point.tps ?? 0));
    const countMax = Math.max(1, Math.ceil(Math.max(...points.map(point => point.concurrency ?? 0))));
    const y = value => lineBase - value / yMax * (lineBase - 20);
    const style = getComputedStyle(root);
    const blue = style.getPropertyValue('--trend-blue').trim();
    const red = style.getPropertyValue('--trend-red').trim();
    const surface = style.getPropertyValue('--surface').trim();
    const selected = points.findIndex(point => point.end === selectedEnd);

    for (const [index, point] of points.entries()) {
      if (point.concurrency === null) continue;
      const bar = point.concurrency / countMax * barHeight;
      const barWidth = Math.max(1, step - .5);
      context.fillStyle = index === selected ? '#a9c9ef' : '#697688';
      context.fillRect(x(index) - barWidth / 2, barBase - bar, barWidth, bar);
    }
    context.lineWidth = 1;
    context.strokeStyle = '#555b68';
    guide(padding, width - padding, lineBase, lineBase);
    guide(padding, width - padding, barBase, barBase);
    if (selected >= 0) {
      context.strokeStyle = '#9ba3b0';
      guide(x(selected), x(selected), 13, barBase);
    }

    const valid = points.filter(point => point.tps !== null);
    for (const label of [peakLabel, floorLabel, meanLabel]) label.hidden = !valid.length;
    if (!valid.length) return;
    const peak = Math.max(...valid.map(point => point.tps));
    const floor = Math.min(...valid.map(point => point.tps));
    const mean = valid.reduce((sum, point) => sum + point.tps, 0) / valid.length;
    const flat = peak === floor;
    peakLabel.hidden = flat;
    floorLabel.hidden = flat;
    meanLabel.textContent = Math.round(mean);
    meanLabel.style.top = `${y(mean)}px`;
    const meanLineStart = meanLabel.offsetWidth ? meanLabel.offsetWidth + 5 : padding;
    if (meanLineStart < width - padding) {
      context.strokeStyle = '#9ba3b0';
      context.setLineDash([3, 3]);
      guide(meanLineStart, width - padding, y(mean), y(mean));
      context.setLineDash([]);
    }

    context.strokeStyle = blue;
    context.lineWidth = 1.6;
    context.lineJoin = 'round';
    context.lineCap = 'round';
    context.beginPath();
    let connected = false;
    for (const [index, point] of points.entries()) {
      if (point.tps === null) { connected = false; continue; }
      if (connected) context.lineTo(x(index), y(point.tps));
      else context.moveTo(x(index), y(point.tps));
      connected = true;
    }
    context.stroke();

    function dot(px, py, color, radius) {
      context.beginPath();
      context.arc(px, py, radius, 0, Math.PI * 2);
      context.fillStyle = color;
      context.strokeStyle = surface;
      context.lineWidth = 1;
      context.fill();
      context.stroke();
    }
    for (const [index, point] of points.entries()) {
      if (point.tps !== null && points[index - 1]?.tps == null && points[index + 1]?.tps == null) {
        dot(x(index), y(point.tps), blue, 2);
      }
    }
    if (selected >= 0 && points[selected].tps !== null) dot(x(selected), y(points[selected].tps), '#e8edf6', 3.5);
    for (const [label, value, labelY, color] of [
      [peakLabel, peak, 0, red], [floorLabel, floor, lineBase + 2, blue],
    ]) {
      if (label.hidden) continue;
      const index = points.findLastIndex(point => point.tps === value);
      const px = x(index);
      label.textContent = Math.round(value);
      const halfWidth = label.offsetWidth / 2;
      label.style.left = `${Math.max(halfWidth, Math.min(width - halfWidth, px))}px`;
      label.style.top = `${labelY}px`;
      context.strokeStyle = color;
      context.lineWidth = 1;
      guide(px, px, label === peakLabel ? 13 : labelY, y(value));
      dot(px, y(value), color, 3.5);
    }
  }

  function select(index) {
    const point = points[index];
    selectedEnd = point?.end ?? null;
    readout.hidden = !point;
    if (point) {
      const speed = point.tps === null ? 'TPS —' : `${Math.round(point.tps)} TPS`;
      const count = point.concurrency === null ? '—' : point.concurrency.toFixed(1);
      readout.querySelector('strong').textContent = `${speed} · 并发 ${count}`;
      readout.querySelector('span').textContent = `${time.format(point.end - windowMs)}–${time.format(point.end)}`;
    }
    paint();
  }

  function selectPointer(event) {
    const rect = canvas.getBoundingClientRect();
    const slot = Math.floor((event.clientX - rect.left - padding) / (rect.width - 2 * padding) * slots);
    select(Math.min(points.length - 1, slot - slots + points.length));
  }

  plot.addEventListener('pointermove', selectPointer);
  plot.addEventListener('pointerdown', selectPointer);
  plot.addEventListener('pointerleave', () => { if (document.activeElement !== plot) select(-1); });
  plot.addEventListener('focus', () => { if (selectedEnd === null) select(points.length - 1); });
  plot.addEventListener('blur', () => select(-1));
  plot.addEventListener('keydown', event => {
    const at = points.findIndex(point => point.end === selectedEnd);
    const target = { ArrowLeft: Math.max(0, at - 1), ArrowRight: Math.min(points.length - 1, at + 1), Home: 0, End: points.length - 1, Escape: -1 };
    if (!Object.hasOwn(target, event.key)) return;
    event.preventDefault();
    select(target[event.key]);
  });
  new ResizeObserver(paint).observe(canvas);

  window.hudLiveTrend = {
    update(history) {
      points = history;
      root.hidden = points.length === 0;
      select(points.findIndex(point => point.end === selectedEnd));
    },
  };
})();
