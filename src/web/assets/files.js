import { activeFilterChips } from './ui-model.js';

function inactiveValue(key) {
  return key === 'enabled' ? null : '';
}

export function syncFilterOptions(select, values, selected = select.value) {
  const first = select.options?.[0] || select.children?.[0] || null;
  const document = select.ownerDocument || select.document;
  const normalized = [...new Set(values.filter(Boolean))]
    .sort((left, right) => left.localeCompare(right, undefined, {
      sensitivity: 'base',
      numeric: true,
    }));
  const options = normalized.map((value) => {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = value;
    return option;
  });
  select.replaceChildren(...(first ? [first, ...options] : options));
  select.value = normalized.includes(selected) ? selected : '';
}

export function mountFilterControls({
  document,
  surface,
  filters,
  labels,
  keys,
  onChange,
  folderValue,
  clearFolder,
}) {
  const controls = new Map(keys.map((key) => [
    key,
    document.querySelector(`#${surface}-filter-${key}`),
  ]));
  const chips = document.querySelector(`#${surface}-filter-chips`);
  const clearAll = document.querySelector(`#${surface}-filters-clear`);

  function readControl(key, control) {
    if (key !== 'enabled') return control.value;
    return control.value === '' ? null : control.value === 'true';
  }

  function render() {
    const values = { ...filters };
    const folder = folderValue?.();
    if (folder) values.folder = folder;
    const descriptors = activeFilterChips(values, labels);
    chips.replaceChildren(...descriptors.map(({ key, label }) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'filter-chip';
      button.textContent = `${label} ×`;
      button.setAttribute('aria-label', `Remove ${label} filter`);
      button.onclick = () => {
        if (key === 'folder') clearFolder?.();
        else {
          filters[key] = inactiveValue(key);
          const control = controls.get(key);
          if (control) control.value = '';
        }
        onChange();
      };
      return button;
    }));
    chips.hidden = descriptors.length === 0;
    clearAll.hidden = descriptors.length === 0;
  }

  for (const [key, control] of controls) {
    control.onchange = () => {
      filters[key] = readControl(key, control);
      onChange();
    };
  }
  clearAll.onclick = () => {
    for (const [key, control] of controls) {
      filters[key] = inactiveValue(key);
      control.value = '';
    }
    clearFolder?.();
    onChange();
  };

  return Object.freeze({
    render,
    setOptions(key, values) {
      const control = controls.get(key);
      if (control) syncFilterOptions(control, values, filters[key] || '');
    },
  });
}
