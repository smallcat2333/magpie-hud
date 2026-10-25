'use strict';

// 短动画只在数据改变时运行；旧节点不参与布局、交互或辅助技术朗读。
(() => {
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');
  const easing = 'cubic-bezier(.22,1,.36,1)';
  const enabled = () => !reduced.matches && !document.hidden;
  const exits = new Set();

  function animate(node, frames, duration = 320) {
    return node.animate(frames, { duration, easing });
  }

  function capture(nodes, key) {
    return new Map([...nodes].map(node => {
      const style = getComputedStyle(node);
      const movement = node.getAnimations().find(animation => animation.playState === 'running'
        && animation.effect?.getKeyframes().some(frame => frame.transform !== undefined));
      const rect = node.getBoundingClientRect();
      const transform = new DOMMatrixReadOnly(style.transform === 'none' ? undefined : style.transform);
      const motion = movement ? { frames: movement.effect.getKeyframes(), timing: movement.effect.getTiming(),
        time: movement.currentTime, left: rect.left - transform.m41, top: rect.top - transform.m42 } : null;
      return [key(node), { rect, state: node.dataset.status,
        border: style.borderColor, opacity: Number(style.opacity), motion,
        clone: enabled() ? node.cloneNode(true) : null }];
    }));
  }

  function layout(before, nodes, key) {
    if (!enabled()) return false;
    let changed = false;
    const current = new Set();
    for (const node of nodes) {
      const id = key(node);
      current.add(id);
      const previous = before.get(id);
      const rect = node.getBoundingClientRect();
      node.getAnimations().forEach(animation => animation.cancel());
      if (!previous) {
        animate(node, [{ opacity: 0, transform: 'translateY(6px)' }, { opacity: 1, transform: 'none' }], 220);
        changed = true;
      } else {
        if (previous.state && previous.state !== node.dataset.status) {
          animate(node, [{ borderColor: previous.border }, { borderColor: getComputedStyle(node).borderColor }], 240);
          const tag = node.querySelector('.state-tag');
          if (tag) animate(tag, [{ opacity: .35, transform: 'translateY(3px)' }, { opacity: 1, transform: 'none' }], 220);
        }
        const dx = previous.rect.left - rect.left;
        const dy = previous.rect.top - rect.top;
        const motion = previous.motion;
        if (motion && Math.abs(motion.left - rect.left) < .5 && Math.abs(motion.top - rect.top) < .5) {
          // 快照重建节点时接续原动画时间，避免每次刷新都重新缓入。
          const continuation = node.animate(motion.frames, motion.timing);
          continuation.currentTime = motion.time;
          changed = true;
        } else if (Math.abs(dx) > .5 || Math.abs(dy) > .5) {
          animate(node, [{ opacity: previous.opacity, transform: `translate(${dx}px,${dy}px)` },
            { opacity: 1, transform: 'none' }], 280);
          changed = true;
        }
      }
    }
    for (const [id, previous] of before) {
      if (current.has(id) || !previous.clone || previous.rect.width === 0) continue;
      const { clone, rect } = previous;
      clone.querySelectorAll('[id]').forEach(node => node.removeAttribute('id'));
      clone.removeAttribute('id');
      clone.classList.add('motion-exit');
      clone.setAttribute('aria-hidden', 'true');
      clone.inert = true;
      Object.assign(clone.style, { position: 'fixed', left: `${rect.left}px`, top: `${rect.top}px`, width: `${rect.width}px`, height: `${rect.height}px`, margin: '0', minHeight: '0', zIndex: '15', pointerEvents: 'none' });
      const layer = document.createElement('div');
      layer.className = 'motion-exit session-group';
      Object.assign(layer.style, { position: 'fixed', inset: '0', pointerEvents: 'none', zIndex: '15' });
      layer.setAttribute('aria-hidden', 'true');
      layer.inert = true;
      if (clone.classList.contains('subagent-row')) {
        const list = document.createElement('div');
        list.className = 'subagent-list';
        list.append(clone);
        layer.append(list);
      } else layer.append(clone);
      document.getElementById('hud').append(layer);
      exits.add(layer);
      const animation = animate(clone, [{ opacity: previous.opacity, transform: 'none' }, { opacity: 0, transform: 'translateY(-4px)' }], 160);
      const remove = () => { layer.remove(); exits.delete(layer); };
      animation.onfinish = remove;
      animation.oncancel = remove;
      setTimeout(remove, 240);
      changed = true;
    }
    return changed;
  }

  function roll(node, value) {
    const previous = node.dataset.motionValue ?? node.textContent;
    node.dataset.motionValue = value;
    if (previous === value) return;
    node.getAnimations({ subtree: true }).forEach(animation => animation.cancel());
    node.textContent = value;
    if (!enabled() || !previous || previous === '—') return;
    const reel = document.createElement('span');
    reel.className = 'metric-reel';
    reel.setAttribute('aria-hidden', 'true');
    const label = document.createElement('span');
    label.className = 'metric-readable';
    label.textContent = value;
    const offset = previous.length - value.length;
    [...value].forEach((character, index) => {
      const before = previous[index + offset] || '';
      if (before === character || !/\d/.test(character)) {
        reel.append(document.createTextNode(character));
        return;
      }
      const cell = document.createElement('span');
      cell.className = 'metric-digit';
      const old = document.createElement('span');
      old.className = 'metric-digit-old';
      old.textContent = before;
      const next = document.createElement('span');
      next.className = 'metric-digit-new';
      next.textContent = character;
      cell.append(old, next);
      reel.append(cell);
    });
    node.replaceChildren(reel, label);
    reel.querySelectorAll('.metric-digit').forEach(cell => {
      const old = cell.querySelector('.metric-digit-old');
      const next = cell.querySelector('.metric-digit-new');
      const outgoing = animate(old, [{ transform: 'translateY(0)' }, { transform: 'translateY(100%)' }], 360);
      outgoing.effect.updateTiming({ fill: 'forwards' });
      animate(next, [{ transform: 'translateY(-100%)' }, { transform: 'translateY(0)' }], 360);
    });
    const finish = () => { if (reel.isConnected) node.replaceChildren(document.createTextNode(value)); };
    setTimeout(finish, 400);
  }

  function settle() {
    for (const node of exits) node.remove();
    exits.clear();
    document.getAnimations().forEach(animation => {
      if (animation.effect?.target?.closest('.metric,.quota-item,.routes,.hourly-chart')) {
        // CSS 的运行/等待动画保持原速，只终止这层有限过渡。
        if (animation.effect.getTiming().iterations !== Infinity) animation.finish();
      }
    });
  }
  reduced.addEventListener('change', () => { if (reduced.matches) settle(); });
  document.addEventListener('visibilitychange', () => { if (document.hidden) settle(); });
  window.hudMotion = { enabled, animate, capture, layout, roll };
})();
