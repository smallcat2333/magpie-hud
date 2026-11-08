'use strict';

// 页面只通过 HudData 取数。桌面壳的 invoke 仍进 Tauri；浏览器走同源 /api/v1。
// 壳操作（拖动、缩放、置顶、隐藏、关闭、打开详情窗）不经过这里。
(() => {
  const shell = Boolean(window.__TAURI__?.core?.invoke);
  const preview = !shell && new URLSearchParams(location.search).get('preview') === '1';
  const handlers = new Map();
  const shellBound = new Map();
  const forwardEvents = new Set(['route-update', 'config-error', 'usage-details-context']);
  let generation = 0;
  let sourceKey = '';
  let forcedSource = 0;
  let notedRustGeneration = null;
  let connectionError = '';
  let lastRevision = -Infinity;
  let envelope = null;
  let capabilities = null;
  let serverToday = '';
  let serverTimeZone = '';
  // serverNow 是后端采集时刻（RFC3339/毫秒），用于校准客户端时钟偏差；
  // 缺省按实时 Date.now，偏差只在远端返回后生效。
  let serverNowOffset = 0;
  let serverNowKnown = false;
  let stale = false;
  let es = null;
  let streamWanted = false;
  let startupSettled = false;
  let loginTask = null;
  let loginOpen = false;
  let authProbe = false;

  document.documentElement.dataset.hudShell = shell ? 'desktop' : 'browser';
  if (preview) document.documentElement.dataset.hudPreview = '1';

  function fail(message, fields) {
    const error = new Error(message);
    if (fields) Object.assign(error, fields);
    error.toString = () => message;
    return error;
  }

  function staleError() {
    return fail('数据源已切换', { hudStale: true });
  }

  function noteToday(value) {
    if (typeof value === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(value)) serverToday = value;
    return serverToday;
  }

  // 服务器时钟校准：远端收集时刻与本地接收时刻相减，作为客户端时钟偏差。
  // 只接受有限时间戳；无效值不改变既有校准。
  function noteServerNow(value) {
    let stamp = null;
    if (typeof value === 'number' && Number.isFinite(value)) stamp = value;
    else if (typeof value === 'string' && value) {
      const parsed = Date.parse(value);
      if (Number.isFinite(parsed)) stamp = parsed;
    }
    if (stamp === null) return;
    // 服务器时间戳减去本地接收时刻，得到本地相对服务器的偏移。
    serverNowOffset = stamp - Date.now();
    serverNowKnown = true;
  }

  function noteClockSource(payload) {
    if (!payload || typeof payload !== 'object') return;
    // 支持多种字段名；capabilities 与快照信封都可能携带采集时刻。
    for (const key of ['serverNow', 'serverTime', 'now', 'collectedAt', 'time']) {
      if (payload[key] !== undefined) { noteServerNow(payload[key]); break; }
    }
  }

  function now() {
    return Date.now() + serverNowOffset;
  }

  function noteSnapshotDate(snapshot) {
    if (!snapshot || typeof snapshot !== 'object') return serverToday;
    if (typeof snapshot.timezone === 'string' && snapshot.timezone) serverTimeZone = snapshot.timezone;
    noteClockSource(snapshot);
    return noteToday(snapshot.latestDate || snapshot.date);
  }

  function noteCapabilities(caps) {
    capabilities = caps || null;
    if (typeof caps?.timezone === 'string' && caps.timezone) serverTimeZone = caps.timezone;
    noteClockSource(caps);
    return capabilities;
  }

  function emit(event, payload) {
    if (event === 'route-update') noteSnapshotDate(payload);
    for (const handler of handlers.get(event) || []) handler(payload);
  }

  function setStale(value) {
    if (stale === value) return;
    stale = value;
    document.documentElement.dataset.hudStream = value ? 'stale' : 'live';
    emit('stale', value);
  }

  // 桌面连接或浏览器实例变化时递增 generation，在途查询不得再写回。
  function noteSource(payload, force = false) {
    const body = payload && typeof payload === 'object' ? payload : {};
    const known = ['instanceId', 'bootId', 'mode', 'remoteUrl'].some(key => body[key] != null && String(body[key]) !== '');
    const ident = known
      ? `desktop|${body.mode || ''}|${body.remoteUrl || ''}|${body.instanceId || ''}|${body.bootId || ''}`
      : `desktop-event|${++forcedSource}`;
    const rustGen = Number(body.generation);
    const genChanged = Number.isFinite(rustGen) && rustGen !== notedRustGeneration;
    if (Number.isFinite(rustGen)) notedRustGeneration = rustGen;
    if (!force && ident === sourceKey && !genChanged) return;
    sourceKey = ident;
    generation += 1;
    serverToday = '';
    serverTimeZone = typeof body.timezone === 'string' ? body.timezone : '';
    serverNowOffset = 0;
    serverNowKnown = false;
    emit('source-change', {
      generation, sourceKey,
      mode: body.mode || '', remoteUrl: body.remoteUrl || '',
      instanceId: body.instanceId || '', bootId: body.bootId || '',
    });
  }

  // 先按 instanceId/bootId 分代，再按 revision 丢掉重复和更旧的信封。
  function deliver(env, notify) {
    if (!env || typeof env !== 'object' || !env.snapshot || typeof env.snapshot !== 'object') return false;
    const instanceId = env.instanceId == null ? '' : String(env.instanceId);
    const bootId = env.bootId == null ? '' : String(env.bootId);
    const ident = `browser|${instanceId}|${bootId}`;
    if (ident !== sourceKey) {
      sourceKey = ident;
      generation += 1;
      lastRevision = -Infinity;
      emit('source-change', { generation, sourceKey, instanceId, bootId });
    }
    const rev = Number(env.revision);
    if (Number.isFinite(rev)) {
      if (!(rev > lastRevision)) return false;
      lastRevision = rev;
    }
    envelope = env;
    // 信封自己的 serverNow/collectedAt 比快照内部字段更可靠。
    if (typeof env.timezone === 'string' && env.timezone) serverTimeZone = env.timezone;
    noteClockSource(env);
    noteSnapshotDate(env.snapshot);
    if (notify) emit('route-update', env.snapshot);
    return true;
  }

  async function readResponse(res) {
    const text = await res.text();
    let body = null;
    if (text) {
      try { body = JSON.parse(text); } catch { body = null; }
    }
    if (!res.ok) {
      const message = body && typeof body.error === 'string' && body.error.trim()
        ? body.error.trim()
        : `请求失败（${res.status}）`;
      throw fail(message, { status: res.status, hudAuth: res.status === 401 });
    }
    return body;
  }

  async function request(url, options) {
    let res;
    try {
      res = await fetch(url, {
        credentials: 'same-origin',
        cache: 'no-store',
        headers: { accept: 'application/json', ...(options?.body ? { 'content-type': 'application/json' } : {}) },
        ...options,
      });
    } catch {
      throw fail('无法连接服务器');
    }
    return readResponse(res);
  }

  const getJson = url => request(url);
  const postJson = (url, body) => request(url, { method: 'POST', body: JSON.stringify(body ?? {}) });

  function desktopInvoke(command, args) {
    return args === undefined
      ? window.__TAURI__.core.invoke(command)
      : window.__TAURI__.core.invoke(command, args);
  }

  function browserInvoke(command, args) {
    if (!/^[a-z][a-z0-9_]*$/.test(command)) throw fail('未知查询');
    // 浏览器走分页概览；旧版服务端没有该命令时给出明确升级提示。
    const mapped = command === 'query_usage_details' ? 'query_usage_details_overview' : command;
    return postJson(`/api/v1/query/${mapped}`, args ?? {}).catch(error => {
      if (mapped !== command && /未知查询/.test(String(error?.message || ''))) {
        throw fail('远端 HUD 版本过旧，不支持用量详情分页。请升级到 1.0.32 或更新版本。');
      }
      throw error;
    });
  }

  function bindShellEvent(event) {
    if (shellBound.has(event)) return shellBound.get(event);
    const pending = window.__TAURI__.event.listen(event, message => emit(event, message?.payload))
      .catch(error => { shellBound.delete(event); throw error; });
    shellBound.set(event, pending);
    return pending;
  }

  function noteConnection(payload) {
    const body = payload && typeof payload === 'object' ? payload : {};
    noteClockSource(body);
    if (typeof body.timezone === 'string' && body.timezone) serverTimeZone = body.timezone;
    if (body.connected === false) {
      connectionError = typeof body.error === 'string' && body.error ? body.error : '远端已断开 · 最后数据';
      setStale(true);
    } else if (body.connected === true) {
      connectionError = '';
      setStale(false);
    }
    emit('connection-status', body);
  }

  if (shell) {
    window.__TAURI__.event.listen('source-change', message => noteSource(message?.payload)).catch(() => {});
    window.__TAURI__.event.listen('connection-status', message => noteConnection(message?.payload)).catch(() => {});
  }

  function connectEvents() {
    if (typeof EventSource !== 'function') throw fail('这个浏览器不能接收实时数据');
    if (es) {
      es.onerror = null;
      es.close();
      es = null;
    }
    const stream = new EventSource('/api/v1/events');
    es = stream;
    stream.addEventListener('snapshot', event => {
      if (es !== stream) return;
      let data;
      try { data = JSON.parse(event.data); } catch { return; }
      setStale(false);
      deliver(data, true);
    });
    stream.addEventListener('auth-expired', () => {
      if (es !== stream) return;
      stream.onerror = null;
      stream.close();
      if (es === stream) es = null;
      requireLogin().catch(() => {});
    });
    stream.onopen = () => { if (es === stream) setStale(false); };
    stream.onerror = () => {
      if (es !== stream) return;
      setStale(true);
      probeAuth(stream);
    };
  }

  async function probeAuth(stream) {
    if (loginOpen || authProbe || es !== stream) return;
    authProbe = true;
    await new Promise(resolve => setTimeout(resolve, 400));
    authProbe = false;
    if (loginOpen || es !== stream) return;
    try {
      const res = await fetch('/api/v1/capabilities', { credentials: 'same-origin', cache: 'no-store', headers: { accept: 'application/json' } });
      if (res.status === 401 && es === stream) {
        stream.onerror = null;
        stream.close();
        if (es === stream) es = null;
        await requireLogin();
      }
    } catch { /* 断线保留已有数据，事件源会自行重连。 */ }
  }

  function ensureLoginForm() {
    let root = document.getElementById('hud-login');
    if (root) return root;
    const style = document.createElement('style');
    style.textContent = '#hud-login{position:fixed;inset:0;z-index:100;display:grid;place-items:center;padding:16px;background:rgb(23 25 30/.72)}#hud-login[hidden]{display:none!important}#hud-login form{width:min(320px,100%);display:grid;gap:10px;padding:16px;background:#17191e;color:#f2f0f2;border:1px solid rgb(190 192 205/.2);border-radius:8px}#hud-login h2,#hud-login p{margin:0}#hud-login h2{font-size:16px;font-weight:600}#hud-login p{color:#b4b6bf;font-size:12px;line-height:1.5}#hud-login label{display:grid;gap:6px;font-size:12px;color:#b4b6bf}#hud-login input{height:32px;padding:0 8px;color:#f2f0f2;background:#1d2025;border:1px solid #555761;border-radius:5px;font:inherit;user-select:text}#hud-login button{height:32px;border:0;border-radius:5px;background:#2a3544;color:#f2f0f2;font:inherit;cursor:pointer}#hud-login [role=status]{min-height:1.2em;color:#e2b67a}';
    document.head.append(style);
    root = document.createElement('div');
    root.id = 'hud-login';
    root.hidden = true;
    const form = document.createElement('form');
    const title = document.createElement('h2');
    title.textContent = '登录 Magpie HUD';
    const note = document.createElement('p');
    note.textContent = '输入访问密钥。密钥只提交给当前站点，不会写进本地存储。';
    const label = document.createElement('label');
    label.textContent = '访问密钥';
    const input = document.createElement('input');
    input.type = 'password';
    input.autocomplete = 'off';
    input.spellcheck = false;
    input.setAttribute('aria-label', '访问密钥');
    label.append(input);
    const submit = document.createElement('button');
    submit.type = 'submit';
    submit.textContent = '登录';
    const status = document.createElement('p');
    status.setAttribute('role', 'status');
    form.append(title, note, label, submit, status);
    root.append(form);
    document.body.append(root);
    return root;
  }

  function promptLogin() {
    const root = ensureLoginForm();
    const form = root.querySelector('form');
    const input = root.querySelector('input');
    const status = root.querySelector('[role=status]');
    root.hidden = false;
    input.value = '';
    status.textContent = '';
    input.focus();
    return new Promise(resolve => {
      form.onsubmit = async event => {
        event.preventDefault();
        const key = input.value;
        if (!key.trim()) {
          status.textContent = '请输入访问密钥';
          return;
        }
        status.textContent = '正在登录…';
        try {
          await postJson('/api/v1/login', { key });
          input.value = '';
          status.textContent = '';
          root.hidden = true;
          form.onsubmit = null;
          resolve();
        } catch (error) {
          input.value = '';
          status.textContent = error?.message || '登录失败';
        }
      };
    });
  }

  async function resumeBrowser(notify) {
    if (!streamWanted) return;
    connectEvents();
    try {
      deliver(await getJson('/api/v1/snapshot'), notify);
    } catch (error) {
      if (!envelope || error?.hudAuth) throw error;
    }
  }

  function requireLogin() {
    if (shell || preview) return Promise.reject(fail('需要登录', { hudAuth: true }));
    if (!loginTask) {
      loginOpen = true;
      loginTask = (async () => {
        if (es) {
          es.onerror = null;
          es.close();
          es = null;
        }
        await promptLogin();
        try { noteCapabilities(await getJson('/api/v1/capabilities')); } catch { capabilities = null; }
        if (startupSettled) await resumeBrowser(true);
      })().finally(() => {
        loginOpen = false;
        loginTask = null;
      });
    }
    return loginTask;
  }

  async function ensureSession() {
    if (shell || preview) return capabilities;
    try {
      noteCapabilities(await getJson('/api/v1/capabilities'));
    } catch (error) {
      if (!error?.hudAuth) throw error;
      await requireLogin();
      if (!capabilities) noteCapabilities(await getJson('/api/v1/capabilities'));
    }
    return capabilities;
  }

  async function invoke(command, args) {
    if (preview) throw fail('预览模式不查询数据', { hudPreview: true });
    const gen = generation;
    const run = () => shell ? desktopInvoke(command, args) : browserInvoke(command, args);
    try {
      const result = await run();
      if (gen !== generation) throw staleError();
      return result;
    } catch (error) {
      if (shell || !error?.hudAuth) throw error;
      await requireLogin();
      if (gen !== generation) throw staleError();
      const result = await run();
      if (gen !== generation) throw staleError();
      return result;
    }
  }

  async function listen(event, handler) {
    const set = handlers.get(event) || new Set();
    handlers.set(event, set);
    set.add(handler);
    if (shell && forwardEvents.has(event)) await bindShellEvent(event);
    return () => set.delete(handler);
  }

  function noteDesktopState(state) {
    if (typeof state?.timezone === 'string' && state.timezone) serverTimeZone = state.timezone;
    noteClockSource(state);
    return noteSnapshotDate(state?.snapshot);
  }

  async function refreshDesktopSnapshot() {
    try {
      const state = await desktopInvoke('startup_state');
      noteDesktopState(state);
      if (state?.snapshot) emit('route-update', state.snapshot);
    } catch { /* 后端随后的 route-update 仍会补上。 */ }
  }

  async function serverTodayValue() {
    if (!shell && !preview && !capabilities) await ensureSession();
    if (serverToday) return serverToday;
    if (preview) return '';
    if (shell) {
      const state = await desktopInvoke('startup_state');
      return noteDesktopState(state);
    }
    try {
      return noteSnapshotDate((await getJson('/api/v1/snapshot'))?.snapshot);
    } catch (error) {
      if (!error?.hudAuth) throw error;
      await requireLogin();
      return noteSnapshotDate((await getJson('/api/v1/snapshot'))?.snapshot);
    }
  }

  async function startup() {
    if (preview) return { snapshot: null, pinned: false, preview: true };
    if (shell) {
      const state = await desktopInvoke('startup_state');
      noteDesktopState(state);
      return state;
    }
    streamWanted = true;
    await ensureSession();
    connectEvents();
    try {
      deliver(await getJson('/api/v1/snapshot'), false);
    } catch (error) {
      if (!envelope) {
        if (!error?.hudAuth) throw error;
        await requireLogin();
        deliver(await getJson('/api/v1/snapshot'), false);
      }
    }
    startupSettled = true;
    if (streamWanted && !es) connectEvents();
    return { snapshot: envelope?.snapshot || null, pinned: false, browser: true };
  }

  function desktopOnly() {
    if (!shell) throw fail('只有桌面端可以修改连接');
  }

  async function connectionSettings() {
    if (!shell) {
      return {
        mode: 'browser',
        remoteUrl: location.origin,
        hasCredential: false,
        instanceId: envelope?.instanceId || '',
        bootId: envelope?.bootId || '',
        revision: envelope?.revision ?? null,
        collectedAt: envelope?.collectedAt || '',
      };
    }
    return desktopInvoke('connection_settings');
  }

  async function testConnection(input) {
    desktopOnly();
    return desktopInvoke('test_connection', {
      remoteUrl: String(input?.remoteUrl || ''),
      key: String(input?.key || ''),
    });
  }

  async function saveConnection(input) {
    desktopOnly();
    const mode = input?.mode === 'remote' ? 'remote' : 'local';
    const settings = await desktopInvoke('save_connection', {
      mode,
      remoteUrl: String(input?.remoteUrl || ''),
      key: mode === 'remote' ? String(input?.key || '') : '',
    });
    noteSource(settings && typeof settings === 'object' ? settings : { mode, remoteUrl: input?.remoteUrl || '' }, true);
    await refreshDesktopSnapshot();
    return settings;
  }

  async function forgetConnection() {
    desktopOnly();
    const settings = await desktopInvoke('forget_connection');
    noteSource(settings && typeof settings === 'object' ? settings : { mode: 'local' }, true);
    await refreshDesktopSnapshot();
    return settings;
  }

  async function logout() {
    if (shell || preview) throw fail('当前环境不能退出网页登录');
    try { await postJson('/api/v1/logout'); }
    catch (error) { if (!error?.hudAuth) throw error; }
    if (es) {
      es.onerror = null;
      es.close();
      es = null;
    }
    setStale(false);
    capabilities = null;
    await requireLogin();
  }

  window.HudData = {
    get shell() { return shell; },
    get preview() { return preview; },
    get generation() { return generation; },
    get stale() { return stale; },
    get envelope() { return envelope; },
    get capabilities() { return capabilities; },
    today() { return serverToday; },
    noteToday,
    serverToday: serverTodayValue,
    timeZone() { return serverTimeZone; },
    // 校准后的服务器毫秒时刻；未知服务器时间时等于 Date.now()。
    now,
    get serverNowKnown() { return serverNowKnown; },
    connectionError() { return connectionError; },
    invoke,
    listen,
    startup,
    ensureSession,
    connectionSettings,
    testConnection,
    saveConnection,
    forgetConnection,
    logout,
  };
})();
