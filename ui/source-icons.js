'use strict';

// 只接收应用内图标路径，JSON 不执行脚本或加载远端图片。
(() => {
  const normalize = value => String(value || '').trim().toLowerCase().replace(/[\s_-]/g, '');
  const validPath = value => typeof value === 'string' && /^icons\/[a-z0-9][a-z0-9_-]*\.(svg|png|webp)$/i.test(value);
  const rules = new Map();
  let fallback = 'icons/robot.svg';
  const ready = fetch('source-icons.json').then(response => {
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return response.json();
  }).then(config => {
    if (config.version !== 1 || !Array.isArray(config.rules)) throw new Error('图标规则格式无效');
    if (validPath(config.fallback)) fallback = config.fallback;
    for (const rule of config.rules) {
      if (!validPath(rule?.icon) || !Array.isArray(rule?.agents)) continue;
      for (const agent of rule.agents) {
        if (typeof agent === 'string' && normalize(agent) && !rules.has(normalize(agent))) rules.set(normalize(agent), rule.icon);
      }
    }
  }).catch(error => console.warn('来源图标规则读取失败，使用默认机器人', error));
  window.hudSourceIcons = { ready, match: agent => rules.get(normalize(agent)), fallback: () => fallback };
})();
