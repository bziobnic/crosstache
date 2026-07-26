// Shared hierarchical tree grid for the secrets and files surfaces.
//
// Folders and their contents live in one table: each row is indented by depth,
// folder rows carry a disclosure chevron, and every row carries a checkbox whose
// state propagates down (checking a folder selects its descendants) and rolls up
// (partial descendant selection renders as indeterminate). Only leaf rows are
// selection targets — a folder checkbox is a proxy for the items beneath it, so
// bulk actions keep operating on the same identifier set they always did.
import { branchSelection, applyBranchSelection } from './ui-model.js';

const NAVIGATION_KEYS = ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End'];

function setCheckboxState(checkbox, state) {
  checkbox.checked = state === 'checked';
  checkbox.indeterminate = state === 'mixed';
  checkbox.setAttribute('aria-checked', state === 'mixed' ? 'mixed' : String(state === 'checked'));
}

export function renderTreeGrid({
  document,
  tbody,
  rows,
  selection = { enabled: false, pending: false, ids: new Set() },
  treeContent,
  cells,
  rowLabel,
  selectionLabel,
  onToggle,
  onSelectionChange,
  onActivate = null,
  focusKey = '',
  onFocus = null,
  markControl = null,
  treeCellClass = '',
}) {
  const selectedIds = selection.ids instanceof Set ? selection.ids : new Set(selection.ids || []);
  const elements = [];
  const byKey = new Map();
  const hadFocus = Boolean(tbody.contains?.(document.activeElement));
  // Restoring focus after a rerender has to land on the same control, not just
  // the same row: toggling a checkbox must leave focus on that checkbox.
  const previousFocusKey = hadFocus ? (document.activeElement?.__xvTreeRowKey || '') : '';
  const previousControl = hadFocus ? (document.activeElement?.__xvTreeControl || 'row') : 'row';
  const controls = new Map();
  let rovingKey = rows.some((row) => row.key === focusKey) ? focusKey : (rows[0]?.key || '');

  const focusRow = (key) => {
    const target = byKey.get(key);
    if (!target) return;
    rovingKey = key;
    for (const candidate of elements) candidate.tabIndex = candidate === target ? 0 : -1;
    target.focus();
    onFocus?.(key);
  };

  for (const [index, row] of rows.entries()) {
    const tr = document.createElement('tr');
    const state = row.type === 'folder'
      ? branchSelection(row.itemIds, selectedIds)
      : (selectedIds.has(row.identifier) ? 'checked' : 'unchecked');
    tr.className = `tree-row tree-row-${row.type}`;
    tr.dataset.treeRow = row.type;
    tr.dataset.treeLevel = String(row.level);
    if (row.type === 'folder') tr.dataset.treePath = row.path;
    tr.__xvTreeRowKey = row.key;
    tr.setAttribute('role', 'row');
    tr.setAttribute('aria-level', String(row.level));
    tr.setAttribute('aria-selected', String(state === 'checked'));
    if (row.type === 'folder') tr.setAttribute('aria-expanded', String(row.expanded));
    if (rowLabel) tr.setAttribute('aria-label', rowLabel(row));
    tr.tabIndex = row.key === rovingKey ? 0 : -1;
    if (state !== 'unchecked') tr.classList.add('selected-row');

    const treeCell = document.createElement('td');
    treeCell.className = `tree-cell item-name ${treeCellClass}`.trim();
    treeCell.setAttribute('role', 'gridcell');
    treeCell.style.setProperty('--tree-depth', String(row.level - 1));
    // The depth padding lives on the cell so the table keeps its own layout
    // algorithm; only the cell's contents are laid out as a flex row.
    const treeCellInner = document.createElement('div');
    treeCellInner.className = 'tree-cell-inner';
    treeCell.appendChild(treeCellInner);

    const disclosure = document.createElement('button');
    disclosure.type = 'button';
    disclosure.className = 'tree-disclosure';
    disclosure.tabIndex = -1;
    disclosure.setAttribute('aria-hidden', 'true');
    disclosure.textContent = row.hasChildren ? (row.expanded ? '▾' : '▸') : '';
    if (!row.hasChildren) disclosure.classList.add('tree-disclosure-empty');
    if (row.hasChildren) {
      disclosure.onclick = (event) => {
        event?.preventDefault?.();
        event?.stopPropagation?.();
        onToggle?.(row, !row.expanded);
      };
    }
    treeCellInner.appendChild(disclosure);

    if (selection.enabled) {
      const checkbox = document.createElement('input');
      checkbox.type = 'checkbox';
      checkbox.className = 'tree-checkbox';
      checkbox.disabled = Boolean(selection.pending);
      setCheckboxState(checkbox, state);
      checkbox.setAttribute('aria-label', selectionLabel ? selectionLabel(row) : '');
      checkbox.__xvTreeRowKey = row.key;
      checkbox.__xvTreeControl = 'selection';
      controls.set(`selection:${row.key}`, checkbox);
      if (markControl && row.type === 'item') markControl(checkbox, row.identifier, 'selection');
      checkbox.onclick = (event) => event?.stopPropagation?.();
      checkbox.onchange = () => {
        applyBranchSelection(selectedIds, row.itemIds, state !== 'checked');
        onSelectionChange?.(row);
      };
      treeCellInner.appendChild(checkbox);
    }

    treeCellInner.appendChild(treeContent(row));
    tr.appendChild(treeCell);
    for (const cell of cells(row)) {
      cell.setAttribute?.('role', 'gridcell');
      tr.appendChild(cell);
    }

    tr.onfocus = () => {
      rovingKey = row.key;
      for (const candidate of elements) candidate.tabIndex = candidate === tr ? 0 : -1;
      onFocus?.(row.key);
    };
    tr.onkeydown = (event) => {
      if (event.altKey || event.ctrlKey || event.metaKey) return;
      const position = elements.indexOf(tr);
      let destinationKey = null;
      if (event.key === 'ArrowDown') destinationKey = rows[Math.min(rows.length - 1, position + 1)]?.key;
      if (event.key === 'ArrowUp') destinationKey = rows[Math.max(0, position - 1)]?.key;
      if (event.key === 'Home') destinationKey = rows[0]?.key;
      if (event.key === 'End') destinationKey = rows.at(-1)?.key;
      if (event.key === 'ArrowRight') {
        if (row.hasChildren && !row.expanded) {
          event.preventDefault();
          onToggle?.(row, true);
          return;
        }
        destinationKey = rows[position + 1]?.key;
      }
      if (event.key === 'ArrowLeft') {
        if (row.hasChildren && row.expanded) {
          event.preventDefault();
          onToggle?.(row, false);
          return;
        }
        destinationKey = row.parentKey;
      }
      if (event.key === ' ' || event.key === 'Spacebar') {
        event.preventDefault();
        if (!selection.enabled || selection.pending) return;
        applyBranchSelection(selectedIds, row.itemIds, state !== 'checked');
        onSelectionChange?.(row);
        return;
      }
      if (event.key === 'Enter') {
        event.preventDefault();
        if (row.type === 'folder') onToggle?.(row, !row.expanded);
        else onActivate?.(row);
        return;
      }
      if (!NAVIGATION_KEYS.includes(event.key)) return;
      event.preventDefault();
      if (destinationKey && byKey.has(destinationKey)) focusRow(destinationKey);
    };
    if (row.type === 'item' && onActivate) {
      tr.onclick = () => { if (!selection.enabled) onActivate(row); };
    } else if (row.type === 'folder') {
      tr.onclick = () => onToggle?.(row, !row.expanded);
    }

    elements.push(tr);
    byKey.set(row.key, tr);
    void index;
  }

  tbody.__xvTreeRows = byKey;
  tbody.replaceChildren(...elements);
  if (hadFocus) {
    const destination = byKey.has(previousFocusKey) ? previousFocusKey : rovingKey;
    const control = controls.get(`${previousControl}:${destination}`);
    if (control) {
      rovingKey = destination;
      for (const candidate of elements) candidate.tabIndex = candidate === byKey.get(destination) ? 0 : -1;
      control.focus();
      onFocus?.(destination);
    } else {
      focusRow(destination);
    }
  }
  return Object.freeze({
    rowKeys: rows.map((row) => row.key),
    focusKey: () => rovingKey,
    rowFor: (key) => byKey.get(key) || null,
  });
}
