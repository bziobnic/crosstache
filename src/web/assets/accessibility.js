export function announce(document, message, priority = 'polite') {
  const liveRegion = document.getElementById('toast');
  if (!liveRegion) return;
  liveRegion.setAttribute('aria-live', priority);
  liveRegion.textContent = message;
}

export function setProtectedValueStatus(document, message) {
  const liveRegion = document.getElementById('protected-value-status');
  if (!liveRegion) return;
  liveRegion.textContent = message;
  liveRegion.hidden = !message;
}

export function setBackgroundInert(document, active) {
  const supportsInert = typeof HTMLElement !== 'undefined' && 'inert' in HTMLElement.prototype;
  for (const element of document.querySelectorAll('header, main, #context-rail')) {
    if (supportsInert) {
      element.inert = active;
    } else if (active) {
      element.setAttribute('aria-hidden', 'true');
    } else {
      element.removeAttribute('aria-hidden');
    }
  }
}

function availableItems(container, selector) {
  return [...(container?.querySelectorAll?.(selector) || [])].filter((item) => (
    !item.hidden
    && !item.disabled
    && !item.closest?.('[hidden]')
    && item.getAttribute?.('aria-disabled') !== 'true'
  ));
}

export function mountRovingFocus(container, selector, {
  orientation = 'horizontal',
  activate = null,
} = {}) {
  const directions = orientation === 'vertical'
    ? new Map([['ArrowDown', 1], ['ArrowUp', -1]])
    : new Map([['ArrowRight', 1], ['ArrowLeft', -1]]);

  function setCurrent(target) {
    const items = availableItems(container, selector);
    for (const item of items) item.tabIndex = item === target ? 0 : -1;
    target?.focus?.();
    return target;
  }

  function onKeydown(event) {
    if (event.defaultPrevented || event.repeat || event.isComposing
      || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    const target = event.target?.closest?.(selector);
    if (!target || !container.contains(target)) return;
    const items = availableItems(container, selector);
    if (!items.length) return;
    const index = items.indexOf(target);
    if (index === -1) return;
    let next = null;
    if (directions.has(event.key)) {
      next = items[(index + directions.get(event.key) + items.length) % items.length];
    } else if (event.key === 'Home') {
      next = items[0];
    } else if (event.key === 'End') {
      next = items.at(-1);
    }
    if (!next) return;
    event.preventDefault();
    setCurrent(next);
    activate?.(next);
  }

  container.addEventListener('keydown', onKeydown);
  return Object.freeze({
    setCurrent,
    items: () => availableItems(container, selector),
    destroy: () => container.removeEventListener('keydown', onKeydown),
  });
}

export function mountTabs(tablist) {
  tablist.setAttribute('role', 'tablist');
  const selector = '[role="tab"]';
  const document = tablist.ownerDocument;

  function activate(tab) {
    return tab.click?.();
  }

  function sync() {
    const tabs = availableItems(tablist, selector);
    const selected = tabs.find((tab) => tab.getAttribute('aria-selected') === 'true') || tabs[0];
    for (const tab of tabs) {
      const active = tab === selected;
      tab.tabIndex = active ? 0 : -1;
      const panel = document?.getElementById?.(tab.getAttribute('aria-controls'));
      if (panel) {
        panel.setAttribute('role', 'tabpanel');
        if (!panel.getAttribute('aria-labelledby')) panel.setAttribute('aria-labelledby', tab.id);
      }
    }
    return selected;
  }

  const roving = mountRovingFocus(tablist, selector, {
    orientation: 'horizontal',
    activate,
  });
  const onClick = () => queueMicrotask(sync);
  tablist.addEventListener('click', onClick);
  sync();

  return Object.freeze({
    sync,
    destroy() {
      roving.destroy();
      tablist.removeEventListener('click', onClick);
    },
  });
}

export function syncVisibleSelection({ visibleIds, selectedIds }) {
  const visible = [...new Set(visibleIds || [])];
  const selected = selectedIds instanceof Set ? selectedIds : new Set(selectedIds || []);
  const selectedVisibleCount = visible.filter((id) => selected.has(id)).length;
  const checked = visible.length > 0 && selectedVisibleCount === visible.length;
  return {
    visibleCount: visible.length,
    selectedVisibleCount,
    checked,
    mixed: selectedVisibleCount > 0 && !checked,
  };
}
