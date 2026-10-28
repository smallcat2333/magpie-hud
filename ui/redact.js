'use strict';
// 账号打码只作用于显示文本：筛选、查询和请求仍使用原始账号。
// 三个窗口共用同一 localStorage 开关，遮罩由原值稳定生成，同一账号处处一致。
(() => {
  const KEY = 'hud-redact-accounts';
  const store = {
    get(key) { try { return localStorage.getItem(key); } catch { return null; } },
    set(key, value) { try { localStorage.setItem(key, value); } catch { /* 无存储时仅本次生效 */ } },
  };
  let enabled = store.get(KEY) === '1';
  const identities = new Set();
  const remember = value => { if (value && !/^(本机|待定|正在选择账号|账号未提供)$/.test(value)) identities.add(String(value)); };
  function scrub(value) {
    let text = String(value ?? '');
    if (!enabled) return text;
    for (const identity of [...identities].sort((a,b) => b.length-a.length)) text = text.split(identity).join(mask(identity));
    return text.replace(/[\w.+-]+@[\w.-]+\.[a-z]{2,}/gi, value => /^u\d{3}@/.test(value) ? value : mask(value));
  }

  /** 原值的稳定序号，只用于挑选固定占位，不进入界面。 */
  function stamp(value) {
    let hash = 0;
    for (const ch of String(value)) hash = (hash * 33 + ch.codePointAt(0)) >>> 0;
    return hash % 1000;
  }

  /** 邮箱保留域名、其余换成定长占位；同一原值始终得到同一结果。 */
  function mask(value) {
    const text = String(value ?? '');
    if (!text) return text;
    const at = text.lastIndexOf('@');
    if (at > 0 && at < text.length - 1 && !text.includes(' ')) return `u${String(stamp(text)).padStart(3, '0')}@${text.slice(at + 1)}`;
    return `账号${String(stamp(text)).padStart(3, '0')}`;
  }

  function apply() {
    document.documentElement.dataset.redact = enabled ? 'on' : 'off';
    document.querySelectorAll('[data-redact]').forEach(button => {
      button.setAttribute('aria-pressed', String(enabled));
      button.title = enabled ? '账号打码已开启' : '账号打码已关闭';
    });
  }

  window.hudRedact = {
    get enabled() { return enabled; },
    mask, remember, scrub,
    /** 开启时遮罩，关闭时原样返回；占位词不遮。 */
    text(value) {
      remember(value);
      return enabled && value && !/^(本机|待定|正在选择账号|账号未提供)$/.test(value) ? mask(value) : value;
    },
    toggle() {
      enabled = !enabled;
      store.set(KEY, enabled ? '1' : '0');
      apply();
      window.dispatchEvent(new Event('hud-redact-change'));
      return enabled;
    },
  };

  window.addEventListener('storage', event => {
    if (event.key !== KEY) return;
    enabled = event.newValue === '1';
    apply();
    window.dispatchEvent(new Event('hud-redact-change'));
  });
  document.addEventListener('DOMContentLoaded', apply);
})();
