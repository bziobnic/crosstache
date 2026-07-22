import { createApiClient } from './api-client.js';
import { createStore, draftReducer } from './store.js';
import { createDialogManager } from './dialogs.js';
import { announce } from './accessibility.js';
import { mountSecrets } from './secrets.js';
import { createPreferenceClient } from './preferences.js';

// Persist the session token per tab, then scrub it from the URL.
const TOKEN_STORAGE_KEY = 'xv.ui.token';
const params = new URLSearchParams(location.search);
const queryToken = params.get('token') || '';
if (queryToken) sessionStorage.setItem(TOKEN_STORAGE_KEY, queryToken);
const token = queryToken || sessionStorage.getItem(TOKEN_STORAGE_KEY) || '';
if (params.has('token')) history.replaceState(null, '', location.pathname);

const api = createApiClient({
  token,
  onInflight: (inflight) => { document.getElementById('progress').hidden = inflight === 0; },
});
const store = createStore({ draft: null, savePending: false }, draftReducer);
const dialogs = createDialogManager(document);
const preferences = createPreferenceClient(api);

mountSecrets({ api, store, dialogs, announce, preferences, token });
