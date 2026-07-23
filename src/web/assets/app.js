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
void preferences.load().then(() => {
  const theme = preferences.get('theme', 'system');
  if (document.documentElement) document.documentElement.dataset.theme = theme;
  document.getElementById('theme-toggle').textContent = `Theme: ${theme}`;
});
const contextRail = mountContextRail({
  store,
  api,
  guardNavigation: () => guardNavigation({
    draft: store.snapshot().draft,
    savePending: store.snapshot().savePending,
    confirmDiscard: () => dialogs.confirmDiscard(),
  }),
});

document.getElementById('commands-open').onclick = () => document.getElementById('search').focus();
document.getElementById('help-open').onclick = () => {
  document.getElementById('context-details').open = true;
  document.getElementById('context-details').focus();
};
document.getElementById('settings-open').onclick = () => {
  document.getElementById('context-details').open = true;
  document.getElementById('theme-toggle').focus();
};
document.getElementById('theme-toggle').onclick = async () => {
  await preferences.load();
  const themes = ['system', 'light', 'dark'];
  const current = preferences.get('theme', 'system');
  const theme = themes[(themes.indexOf(current) + 1) % themes.length];
  preferences.set('theme', theme);
  if (document.documentElement) document.documentElement.dataset.theme = theme;
  document.getElementById('theme-toggle').textContent = `Theme: ${theme}`;
};

mountSecrets({ api, store, dialogs, announce, preferences, token, contextRail });
