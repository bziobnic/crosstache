import { createApiClient } from './api-client.js';
import { createStore, draftReducer } from './store.js';
import { createDialogManager, guardNavigation } from './dialogs.js';
import { announce } from './accessibility.js';
import { mountSecrets } from './secrets.js';
import { createPreferenceClient } from './preferences.js';
import { formatContextLine, mountContextRail } from './context.js';

// Persist the session token per tab, then scrub it from the URL.
const TOKEN_STORAGE_KEY = 'xv.ui.token';
const params = new URLSearchParams(location.search);
const queryToken = params.get('token') || '';
if (queryToken) sessionStorage.setItem(TOKEN_STORAGE_KEY, queryToken);
const token = queryToken || sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
if (params.has('token')) history.replaceState(null, '', location.pathname);

const store = createStore({
  context: null,
  initialSecrets: null,
  contextSwitchPending: false,
  scopedMutationPending: false,
  contextError: null,
  draft: null,
  savePending: false,
}, draftReducer);
const api = createApiClient({
  token,
  onInflight: (inflight) => {
    const progress = document.getElementById('progress');
    progress.hidden = inflight === 0;
    document.getElementById('progress-context').textContent =
      formatContextLine(store.snapshot().context);
  },
});
const dialogs = createDialogManager(document);
const preferences = createPreferenceClient(api);
const themeSelect = document.getElementById('theme-select');

function applyTheme(theme) {
  if (document.documentElement) document.documentElement.dataset.theme = theme;
  if (themeSelect) themeSelect.value = theme;
}

async function retrySettings(button) {
  button.disabled = true;
  try {
    await preferences.retry();
    applyTheme(preferences.get('theme', 'system'));
  } finally {
    button.disabled = false;
  }
}

for (const id of ['settings-retry', 'settings-error-retry']) {
  const button = document.getElementById(id);
  button.onclick = () => retrySettings(button);
}

void preferences.load().then(() => {
  const theme = preferences.get('theme', 'system');
  applyTheme(theme);
});
const contextRail = token ? mountContextRail({
  store,
  api,
  guardNavigation: () => guardNavigation({
    draft: store.snapshot().draft,
    savePending: store.snapshot().savePending,
    confirmDiscard: () => dialogs.confirmDiscard(),
  }),
}) : null;

function bindApplicationDialog({ openId, dialogId, closeId, initialFocus }) {
  const open = document.getElementById(openId);
  const dialog = document.getElementById(dialogId);
  const close = document.getElementById(closeId);
  const dismiss = () => dialogs.closeModal(dialog);
  open.onclick = () => {
    if (store.snapshot().contextSwitchPending) return;
    dialogs.openModal(dialog, {
      initialFocus: initialFocus(),
      invoker: open,
      onEscape: dismiss,
    });
  };
  close.onclick = dismiss;
  return { open, dialog, close: dismiss };
}

const commandsPanel = bindApplicationDialog({
  openId: 'commands-open',
  dialogId: 'commands-dialog',
  closeId: 'commands-close',
  initialFocus: () => document.querySelector(
    '#commands-dialog [data-command-target]:not([hidden]):not([disabled])',
  ),
});
const commandButtons = [
  ...(document.querySelectorAll?.('#commands-dialog [data-command-target]') || []),
];
function syncCommandAvailability() {
  for (const command of commandButtons) {
    const target = document.getElementById(command.dataset.commandTarget);
    command.hidden = !target || target.hidden || target.disabled;
  }
}
const openCommands = commandsPanel.open.onclick;
commandsPanel.open.onclick = () => {
  syncCommandAvailability();
  openCommands();
};
for (const command of commandButtons) {
  const target = document.getElementById(command.dataset.commandTarget);
  command.onclick = async () => {
    commandsPanel.close();
    if (target.id === 'search' || target.id === 'new-secret') {
      await document.getElementById('tab-secrets').onclick();
    }
    if (target.id === 'search') target.focus();
    else target.click();
  };
}

bindApplicationDialog({
  openId: 'help-open',
  dialogId: 'help-dialog',
  closeId: 'help-close',
  initialFocus: () => document.getElementById('help-close'),
});
bindApplicationDialog({
  openId: 'settings-open',
  dialogId: 'settings-dialog',
  closeId: 'settings-close',
  initialFocus: () => themeSelect,
});

document.addEventListener?.('keydown', (event) => {
  if (!(event.key?.toLowerCase() === 'k' && (event.metaKey || event.ctrlKey))) return;
  if (dialogs.topModal()) return;
  event.preventDefault();
  document.getElementById('commands-open').click();
});

themeSelect.onchange = async () => {
  await preferences.load();
  const theme = themeSelect.value;
  preferences.set('theme', theme);
  applyTheme(theme);
};

mountSecrets({ api, store, dialogs, announce, preferences, token, contextRail });
