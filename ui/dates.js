'use strict';

// 日期只控制六项统计与小时图；路由、额度与当日增量账本继续独立刷新。
(() => {
  const byId = id => document.getElementById(id);
  const hud = byId('hud');
  const previous = byId('stats-prev');
  const next = byId('stats-next');
  const todayButton = byId('stats-today');
  const calendar = byId('stats-calendar');
  const daysGrid = byId('calendar-days');
  let calendarMonth = '';
  let calendarView = null;
  let calendarLoading = false;
  let calendarError = '';
  let calendarRequest = 0;
  const monthCache = new Map();
  const monthPending = new Map();
  const status = byId('stats-date-status');
  let live = null;
  let selectedDate = null;
  let shown = null;
  let earliest = '';
  let loading = false;
  let failure = '';
  let requestId = 0;
  let blurredAt = null;
  const cache = new Map();
  const pending = new Map();
  const cacheMs = 60_000;

  function localDate() {
    const now = new Date();
    return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
  }

  function offsetDate(date, days) {
    const [year, month, day] = date.split('-').map(Number);
    const value = new Date(year, month - 1, day + days, 12);
    return `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, '0')}-${String(value.getDate()).padStart(2, '0')}`;
  }

  function latestDate() {
    return live?.date || localDate();
  }

  function dateLabel(date) {
    const isToday = date === latestDate();
    const sameYear = date.slice(0, 4) === latestDate().slice(0, 4);
    return `${sameYear ? date.slice(5) : date}${isToday ? ' 今天' : ''}`;
  }

  function setStatus(message) {
    status.textContent = message;
    status.title = message;
    status.hidden = !message;
  }

  function render() {
    if (!live) return;
    const date = selectedDate || latestDate();
    const historical = Boolean(selectedDate);
    hud.dataset.statsHistory = String(historical);
    hud.dataset.statsLoading = String(loading);
    byId('quota-title').textContent = historical ? '当前额度' : '用量';
    const time = byId('stats-date');
    time.textContent = dateLabel(date);
    time.dateTime = date;
    byId('stats-date-label').title = `${date}，点击查看用量热力日历${historical ? '；历史统计，路由与额度实时更新' : ''}`;
    previous.disabled = !earliest || date <= earliest;
    next.disabled = !historical || date >= latestDate();
    previous.title = previous.disabled ? '已到最早记录' : `前一天 ${offsetDate(date, -1)}`;
    next.title = next.disabled ? '已是今天' : `后一天 ${offsetDate(date, 1)}`;
    todayButton.hidden = !historical;
    byId('stats-retry').hidden = !failure && !(shown?.costError && historical && !loading);
    const region = document.querySelector('.daily');
    region.setAttribute('aria-busy', String(loading));
    region.setAttribute('aria-label', `${date}统计，${live.scope}`);
    document.querySelector('.hourly').setAttribute('aria-busy', String(loading));
    document.querySelector('.hourly').setAttribute('aria-label', `${date}每小时 Token 与金额`);
    byId('hourly-plot').inert = historical && !shown;
    if (historical && !shown) byId('hourly-tooltip').hidden = true;
    let view;
    if (!historical) {
      view = { date: live.date, daily: live.daily, scope: live.scope, costError: live.costError, historical: false };
    } else if (shown) {
      view = { ...shown, scope: '当日全部会话', historical: true };
      if (live.projectHistory) view = { ...live.projectHistory(shown), historical: true };
    }
    if (view) window.dispatchEvent(new CustomEvent('hud-stats-view', { detail: view }));
    // 没有读数时仍保留版式；失败不显示成零，也不把上一个日期的数据当成所选日。
    document.querySelector('.daily dl').inert = historical && !shown;
    hud.dataset.statsUnavailable = String(historical && !shown);
    if (loading) setStatus(`正在读取 ${date}…`);
    else if (failure) setStatus(`${date} 读取失败：${failure}`);
    else if (historical && shown?.costError) setStatus('费用未能读取，其他统计已更新');
    else if (historical && shown?.daily.requests === 0 && !shown?.daily.cost) setStatus('当天暂无记录');
    else setStatus('');
  }

  async function query(date) {
    const cached = cache.get(date);
    if (cached && Date.now() - cached.at < cacheMs) return cached.view;
    if (!pending.has(date)) {
      const task = window.__TAURI__
        ? window.__TAURI__.core.invoke('query_date_stats', { date })
        : Promise.resolve().then(() => {
          // 仅独立预览接受外部 fixture，产品不生成任何演示统计。
          if (!window.hudDatePreviewQuery) throw new Error('预览尚未注入历史数据');
          return window.hudDatePreviewQuery(date);
        });
      pending.set(date, task.then(view => {
        if (view.date !== date) throw new Error('返回日期与所选日期不一致');
        if (!view.costError) {
          cache.set(date, { at: Date.now(), view });
          if (cache.size > 32) cache.delete(cache.keys().next().value);
        }
        return view;
      }).finally(() => pending.delete(date)));
    }
    return pending.get(date);
  }

  async function select(date, force = false) {
    if (!live || !/^\d{4}-\d{2}-\d{2}$/.test(date)) return;
    if (date > latestDate() || earliest && date < earliest) { render(); return; }
    const id = ++requestId;
    failure = '';
    if (date === latestDate()) {
      selectedDate = null;
      shown = null;
      loading = false;
      render();
      return;
    }
    if (date !== selectedDate) shown = null;
    selectedDate = date;
    loading = true;
    if (force) cache.delete(date);
    render();
    try {
      const view = await query(date);
      if (id !== requestId) return;
      shown = view;
      earliest = view.earliestDate || earliest;
    } catch (error) {
      if (id !== requestId) return;
      failure = String(error);
    } finally {
      if (id === requestId) { loading = false; render(); }
    }
  }

  function monthOffset(month, delta) {
    const [year, number] = month.split('-').map(Number);
    const value = new Date(year, number - 1 + delta, 1, 12);
    return `${value.getFullYear()}-${String(value.getMonth() + 1).padStart(2, '0')}`;
  }

  function shortTokens(value) {
    return new Intl.NumberFormat('en', { notation: 'compact', maximumSignificantDigits: 3 }).format(value).replace('K', 'k');
  }

  function positionCalendar() {
    if (calendar.hidden) return;
    const anchor = byId('stats-date-label').getBoundingClientRect();
    calendar.style.width = `${Math.min(280, innerWidth - 16)}px`;
    const bounds = calendar.getBoundingClientRect();
    calendar.style.left = `${Math.max(8, Math.min(anchor.left, innerWidth - bounds.width - 8))}px`;
    const below = innerHeight - anchor.bottom - 8;
    const above = anchor.top - 8;
    const top = below >= bounds.height || below >= above
      ? Math.max(8, Math.min(anchor.bottom + 5, innerHeight - bounds.height - 8))
      : Math.max(8, anchor.top - bounds.height - 5);
    calendar.style.top = `${top}px`;
  }

  function closeCalendar(restoreFocus = true) {
    calendar.hidden = true;
    ++calendarRequest;
    byId('stats-date-label').setAttribute('aria-expanded', 'false');
    if (restoreFocus) byId('stats-date-label').focus({ preventScroll: true });
  }

  function scopedMonth() {
    return calendarView && live?.projectMonth ? live.projectMonth(calendarView) : calendarView;
  }

  function readDay(date) {
    if (date > latestDate()) return `${date} · 尚未到来`;
    if (earliest && date < earliest) return `${date} · 记录开始前`;
    if (calendarLoading) return `${date} · 正在读取用量`;
    if (calendarError || !calendarView) return `${date} · 用量未读取`;
    const item = scopedMonth().days.find(day => day.date === date);
    if (!item) return `${date} · 用量未提供`;
    return `${date} · ${item.tokens.toLocaleString('zh-CN')} Token · ${item.requests.toLocaleString('zh-CN')} 次调用`;
  }

  function renderCalendar() {
    if (calendar.hidden) return;
    const [year, month] = calendarMonth.split('-').map(Number);
    byId('calendar-month').textContent = `${year}年${month}月`;
    byId('calendar-prev').disabled = calendarMonth <= (earliest || latestDate()).slice(0, 7);
    byId('calendar-next').disabled = calendarMonth >= latestDate().slice(0, 7);
    daysGrid.setAttribute('aria-busy', String(calendarLoading));
    const focused = daysGrid.contains(document.activeElement) ? document.activeElement.dataset.date : null;
    const first = new Date(year, month - 1, 1, 12);
    const firstDate = `${calendarMonth}-01`;
    const weekday = (first.getDay() + 6) % 7;
    const count = new Date(year, month, 0).getDate();
    const cells = Math.ceil((weekday + count) / 7) * 7;
    const monthView = scopedMonth();
    const values = new Map((monthView?.days || []).map(day => [day.date, day]));
    const maximum = Math.max(0, ...(monthView?.days || []).filter(day => day.date <= latestDate()).map(day => day.tokens));
    byId('calendar-title').title = live?.agentScopeLabel || '全部会话';
    calendar.setAttribute('aria-label', `用量热力日历：${live?.agentScopeLabel || '全部会话'}`);
    const fragment = document.createDocumentFragment();
    const table = [];
    let total = 0;
    for (let index = 0; index < cells; index++) {
      const date = offsetDate(firstDate, index - weekday);
      const inMonth = date.startsWith(calendarMonth);
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'calendar-day';
      button.dataset.date = date;
      button.textContent = String(Number(date.slice(8)));
      button.dataset.outside = String(!inMonth);
      button.disabled = !inMonth || date > latestDate() || Boolean(earliest && date < earliest);
      const item = values.get(date);
      const known = inMonth && item && !calendarLoading && !calendarError;
      button.dataset.heat = known ? String(item.requests > 0 ? Math.min(4, Math.max(1, maximum > 0 ? Math.ceil(item.tokens / maximum * 4) : 1)) : 0) : 'unknown';
      button.dataset.today = String(date === latestDate());
      const selected = date === (selectedDate || latestDate());
      button.setAttribute('aria-pressed', String(selected));
      if (date === latestDate()) button.setAttribute('aria-current', 'date');
      const detail = readDay(date);
      button.title = detail;
      button.setAttribute('aria-label', `${detail}${selected ? '，已选中' : ''}${date === latestDate() ? '，今天（截至当前）' : ''}`);
      button.addEventListener('pointerenter', () => { byId('calendar-readout').textContent = detail; });
      button.addEventListener('focus', () => { byId('calendar-readout').textContent = detail; });
      button.addEventListener('click', () => { closeCalendar(); select(date); });
      fragment.append(button);
      if (inMonth) {
        const row = document.createElement('tr');
        for (const value of [date, known ? item.tokens.toLocaleString('zh-CN') : '未读取', known ? String(item.requests) : '—']) {
          const cell = document.createElement('td');
          cell.textContent = value;
          row.append(cell);
        }
        table.push(row);
        if (known && date <= latestDate()) total += item.tokens;
      }
    }
    daysGrid.replaceChildren(fragment);
    byId('calendar-table').querySelector('tbody').replaceChildren(...table);
    byId('calendar-summary').textContent = calendarLoading ? '正在读取…' : calendarError ? '读取失败' : `本月 ${shortTokens(total)} Token`;
    byId('calendar-retry').hidden = !calendarError;
    const description = calendarLoading ? '正在读取本月用量…' : calendarError ? calendarError
      : `Token 越多颜色越深，无调用无底色；最高 ${shortTokens(maximum)}，今天截至当前`;
    byId('calendar-readout').textContent = description;
    daysGrid.title = description;
    if (focused) daysGrid.querySelector(`[data-date="${focused}"]`)?.focus({ preventScroll: true });
    positionCalendar();
  }

  async function loadCalendar(month, force = false) {
    if (month < (earliest || latestDate()).slice(0, 7) || month > latestDate().slice(0, 7)) return;
    const id = ++calendarRequest;
    calendarMonth = month;
    calendarView = null;
    calendarError = '';
    if (force) monthCache.delete(month);
    const cached = monthCache.get(month);
    if (cached && Date.now() - cached.at < cacheMs) {
      calendarView = cached.view;
      calendarLoading = false;
      renderCalendar();
      return;
    }
    calendarLoading = true;
    renderCalendar();
    try {
      if (!monthPending.has(month)) {
        const task = window.__TAURI__ ? window.__TAURI__.core.invoke('query_month_usage', { month })
          : Promise.resolve().then(() => {
            if (!window.hudMonthPreviewQuery) throw new Error('预览尚未注入月份数据');
            return window.hudMonthPreviewQuery(month);
          });
        monthPending.set(month, task.then(view => {
          if (view.month !== month || !Array.isArray(view.days)) throw new Error('返回月份与所选月份不一致');
          monthCache.set(month, { at: Date.now(), view });
          if (monthCache.size > 12) monthCache.delete(monthCache.keys().next().value);
          return view;
        }).finally(() => monthPending.delete(month)));
      }
      const view = await monthPending.get(month);
      if (id !== calendarRequest || calendar.hidden) return;
      calendarView = view;
      earliest = view.earliestDate || earliest;
    } catch (error) {
      if (id !== calendarRequest) return;
      calendarError = String(error);
    } finally {
      if (id === calendarRequest && !calendar.hidden) {
        calendarLoading = false;
        renderCalendar();
      }
    }
  }

  byId('calendar-close').addEventListener('click', () => closeCalendar());
  byId('calendar-prev').addEventListener('click', () => loadCalendar(monthOffset(calendarMonth, -1)));
  byId('calendar-next').addEventListener('click', () => loadCalendar(monthOffset(calendarMonth, 1)));
  byId('calendar-retry').addEventListener('click', () => loadCalendar(calendarMonth, true));
  byId('calendar-today').addEventListener('click', () => { closeCalendar(); select(latestDate()); });
  document.addEventListener('pointerdown', event => {
    if (!calendar.hidden && !calendar.contains(event.target) && !byId('stats-date-label').contains(event.target)) closeCalendar(false);
  });
  calendar.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); closeCalendar(); return; }
    const date = event.target.dataset.date;
    if (!date) return;
    const offset = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 }[event.key];
    if (!offset) return;
    event.preventDefault();
    const target = offsetDate(date, offset);
    const button = daysGrid.querySelector(`[data-date="${target}"]`);
    if (button && !button.disabled) button.focus();
  });
  window.addEventListener('resize', positionCalendar);
  document.addEventListener('scroll', positionCalendar, true);

  previous.addEventListener('click', () => select(offsetDate(selectedDate || latestDate(), -1)));
  next.addEventListener('click', () => select(offsetDate(selectedDate || latestDate(), 1)));
  todayButton.addEventListener('click', () => select(latestDate()));
  byId('stats-retry').addEventListener('click', () => select(selectedDate || latestDate(), true));
  byId('stats-date-label').addEventListener('click', () => {
    if (!calendar.hidden) { closeCalendar(); return; }
    if (!live) return;
    calendar.hidden = false;
    byId('stats-date-label').setAttribute('aria-expanded', 'true');
    loadCalendar((selectedDate || latestDate()).slice(0, 7));
    byId('calendar-close').focus({ preventScroll: true });
  });
  byId('stats-date-nav').addEventListener('keydown', event => {
    if (event.key === 'ArrowLeft' && !previous.disabled) {
      event.preventDefault();
      select(offsetDate(selectedDate || latestDate(), -1));
    } else if (event.key === 'ArrowRight' && !next.disabled) {
      event.preventDefault();
      select(offsetDate(selectedDate || latestDate(), 1));
    } else if (event.key === 'Home') {
      event.preventDefault();
      select(latestDate());
    }
  });
  // 仅在离开窗口五分钟后再次聚焦时回到今天，阅读历史时不突然跳页。
  window.addEventListener('blur', () => { blurredAt = Date.now(); });
  window.addEventListener('focus', () => {
    if (selectedDate && blurredAt !== null && Date.now() - blurredAt >= 5 * 60_000) select(latestDate());
    blurredAt = null;
  });
  window.addEventListener('hud-live-stats', event => {
    const old = live;
    live = event.detail;
    earliest = live.earliestDate || earliest || live.date;
    if (old && (old.host !== live.host || old.date !== live.date)) {
      cache.clear();
      monthCache.clear();
      if (!calendar.hidden) loadCalendar((selectedDate || latestDate()).slice(0, 7), true);
    }
    if (old && old.host !== live.host && selectedDate) {
      shown = null;
      select(selectedDate, true);
      return;
    }
    if (selectedDate && (selectedDate < earliest || selectedDate >= latestDate())) {
      ++requestId;
      selectedDate = null;
      shown = null;
      loading = false;
      failure = '';
    }
    render();
    if (!calendar.hidden) renderCalendar();
  });
})();
