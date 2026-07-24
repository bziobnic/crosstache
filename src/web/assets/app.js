import { createApiClient } from './api-client.js';
import { createStore, draftReducer } from './store.js';
import { createDialogManager, guardNavigation } from './dialogs.js';
import { announce, mountTabs } from './accessibility.js';
import { mountSecrets } from './secrets.js';
import { createPreferenceClient } from './preferences.js';
import { formatContextLine, mountContextRail } from './context.js';
import { createCommandRegistry, mountCommandPalette } from './commands.js';

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
if (navigator.webdriver) {
  globalThis.__xvTestStoreSnapshot = () => store.snapshot();
}
const api = createApiClient({
  token,
  onOperation: (event) => store.dispatch({ type: 'operation/status', ...event }),
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
const commandRegistry = createCommandRegistry();
const tabs = mountTabs(document.getElementById('vault-tabs'));

const confirmNavigation = () => guardNavigation({
  draft: store.snapshot().draft,
  savePending: store.snapshot().savePending,
  confirmDiscard: () => dialogs.confirmDiscard(),
});

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
  guardNavigation: confirmNavigation,
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

themeSelect.onchange = async () => {
  await preferences.load();
  const theme = themeSelect.value;
  preferences.set('theme', theme);
  applyTheme(theme);
};

mountSecrets({
  api,
  store,
  dialogs,
  announce,
  preferences,
  token,
  contextRail,
  commandRegistry,
  tabs,
});
mountCommandPalette({
  registry: commandRegistry,
  store,
  dialogs,
  guardNavigation: confirmNavigation,
  activateContext: (alias, options) => contextRail?.switchTo(alias, options),
});
