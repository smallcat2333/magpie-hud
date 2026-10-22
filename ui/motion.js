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
    return new Map([...nodes].map(node => [key(node), { rect: node.getBoundingClientRect(), state: node.dataset.status, border: getComputedStyle(node).borderColor, clone: enabled() ? node.cloneNode(true) : null }]));
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
        animate(node, [{ opacity: 0, transform: 'translateY(12px) scale(.985)' }, { opacity: 1, transform: 'none' }]);
        changed = true;
      } else {
        if (previous.state && previous.state !== node.dataset.status) {
          animate(node, [{ borderColor: previous.border }, { borderColor: getComputedStyle(node).borderColor }], 240);
          const tag = node.querySelector('.state-tag');
          if (tag) animate(tag, [{ opacity: .35, transform: 'translateY(3px)' }, { opacity: 1, transform: 'none' }], 220);
        }
        const dx = previous.rect.left - rect.left;
        const dy = previous.rect.top - rect.top;
        if (Math.abs(dx) > .5 || Math.abs(dy) > .5) {
          animate(node, [{ transform: `translate(${dx}px,${dy}px)` }, { transform: 'none' }]);
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
      const animation = animate(clone, [{ opacity: .65, transform: 'none' }, { opacity: 0, transform: 'translateY(-9px) scale(.985)' }], 180);
      const remove = () => { layer.remove(); exits.delete(layer); };
      animation.onfinish = remove;
      animation.oncancel = remove;
      setTimeout(remove, 240);
      changed = true;
    }
    return changed;
  }

  function flip(node, value) {
    const previous = node.dataset.motionValue ?? node.textContent;
    node.dataset.motionValue = value;
    if (previous === value) return;
    node.getAnimations({ subtree: true }).forEach(animation => animation.cancel());
    node.textContent = value;
    if (!enabled() || !previous || previous === '—') return;
    const old = document.createElement('span');
    old.className = 'metric-outgoing';
    old.textContent = previous;
    old.setAttribute('aria-hidden', 'true');
    const next = document.createElement('span');
    next.className = 'metric-incoming';
    next.textContent = value;
    node.replaceChildren(old, next);
    const outgoing = animate(old, [{ transform: 'perspective(160px) rotateX(0)', opacity: 1 }, { transform: 'perspective(160px) rotateX(80deg) translateY(-5px)', opacity: 0 }], 160);
    outgoing.effect.updateTiming({ fill: 'forwards' });
    const animation = animate(next, [{ transform: 'perspective(160px) rotateX(-80deg) translateY(5px)', opacity: 0 }, { transform: 'perspective(160px) rotateX(0)', opacity: 1 }], 300);
    const finish = () => { if (next.isConnected && node.dataset.motionValue === value) node.replaceChildren(document.createTextNode(value)); };
    animation.onfinish = finish;
    animation.oncancel = finish;
    setTimeout(finish, 340);
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
  window.hudMotion = { enabled, animate, capture, layout, flip };
})();
