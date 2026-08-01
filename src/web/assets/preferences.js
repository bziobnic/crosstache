const LEGACY_WIDTH_SCHEMAS = {
  'xv.ui.columns.secrets.v1': (value) => Array.isArray(value)
    && value.length === 5
    && value.every((width) => Number.isFinite(width)),
  'xv.ui.columns.files.v1': (value) => Array.isArray(value)
    && value.length === 4
    && value.every((width) => Number.isFinite(width)),
};

const DEFAULTS = Object.freeze({
  version: 1,
  theme: 'system',
  exposure_timeout_seconds: 30,
  density: 'comfortable',
  folder_expansion: true,
  column_widths: Object.freeze({
    secrets: Object.freeze([28, 15, 14, 25, 18]),
    files: Object.freeze([42, 12, 24, 22]),
  }),
});

const FIELD_SCHEMAS = {
  theme: (value) => ['system', 'light', 'dark'].includes(value),
  exposure_timeout_seconds: (value) => Number.isSafeInteger(value) && value >= 0,
  density: (value) => ['comfortable', 'compact'].includes(value),
  folder_expansion: (value) => typeof value === 'boolean',
  column_widths: (value) => value !== null
    && typeof value === 'object'
    && LEGACY_WIDTH_SCHEMAS['xv.ui.columns.secrets.v1'](value.secrets)
    && LEGACY_WIDTH_SCHEMAS['xv.ui.columns.files.v1'](value.files),
};

function nonNegativeInteger(value) {
  const number = Number(value);
  return Number.isSafeInteger(number) && number >= 0 ? number : 0;
}

export function boundTimeout(requested, policy) {
  const timeout = nonNegativeInteger(requested);
  const limit = nonNegativeInteger(policy);
  return limit > 0 ? Math.min(timeout, limit) : timeout;
}

function clone(value) {
  return structuredClone(value);
}

function deepFreeze(value) {
  if (value && typeof value === 'object' && !Object.isFrozen(value)) {
    for (const nested of Object.values(value)) deepFreeze(nested);
    Object.freeze(value);
  }
  return value;
}

function immutable(value) {
  return deepFreeze(clone(value));
}

function sanitize(input) {
  const source = input !== null && typeof input === 'object' ? input : {};
  const clean = clone(DEFAULTS);
  for (const [key, isValid] of Object.entries(FIELD_SCHEMAS)) {
    if (isValid(source[key])) clean[key] = clone(source[key]);
  }
  clean.version = 1;
  return clean;
}

function safeError(error) {
  return immutable({
    message: error?.message || 'Settings could not be saved.',
    hint: error?.hint || 'Check the application configuration and try again.',
  });
}

function showSettingsError(error) {
  const document = globalThis.document;
  for (const id of ['settings-status', 'settings-error']) {
    const surface = document?.getElementById?.(id);
    if (!surface) continue;
    const message = surface.querySelector?.('.error-message');
    const hint = surface.querySelector?.('.error-hint');
    if (message) message.textContent = error?.message || '';
    if (hint) hint.textContent = error?.hint || '';
    surface.hidden = !error;
  }
  const opener = document?.getElementById?.('settings-open');
  if (!opener) return;
  if (error) {
    opener.dataset.error = 'true';
    opener.setAttribute('aria-describedby', 'settings-status-message');
  } else {
    delete opener.dataset.error;
    opener.removeAttribute('aria-describedby');
  }
}

function sameValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

export function createPreferenceClient(api, options = {}) {
  const {
    setTimeoutImpl = globalThis.setTimeout.bind(globalThis),
    clearTimeoutImpl = globalThis.clearTimeout.bind(globalThis),
    onSettingsError = showSettingsError,
  } = options;
  let state = immutable(DEFAULTS);
  let loadPromise = null;
  let saveTimer = null;
  let saveQueue = Promise.resolve();
  let retryPromise = null;
  let currentError = null;
  let failedOperation = null;
  const overrides = {};

  function request(method, body) {
    if (typeof api === 'function') return api(method, '/api/preferences', body);
    if (typeof api?.request === 'function') return api.request(method, '/api/preferences', body);
    return Promise.resolve(clone(DEFAULTS));
  }

  function reportError(error, operation) {
    currentError = safeError(error);
    failedOperation = operation;
    onSettingsError?.(currentError);
  }

  function clearReportedError() {
    currentError = null;
    failedOperation = null;
    onSettingsError?.(null);
  }

  function mergeOverrides(base) {
    return sanitize({ ...base, ...overrides });
  }

  function load() {
    if (!loadPromise) {
      loadPromise = request('GET')
        .then((loaded) => {
          state = immutable(mergeOverrides(loaded));
          if (failedOperation === 'load') clearReportedError();
          return immutable(state);
        })
        .catch((error) => {
          reportError(error, 'load');
          return immutable(state);
        });
    }
    return loadPromise;
  }

  async function persist() {
    await load();
    const payload = sanitize(state);
    const sentOverrides = clone(overrides);
    try {
      const saved = await request('PUT', payload);
      for (const [key, value] of Object.entries(sentOverrides)) {
        if (sameValue(overrides[key], value)) delete overrides[key];
      }
      if (currentError !== null) clearReportedError();
      state = immutable(mergeOverrides(saved));
    } catch (error) {
      reportError(error, 'save');
    }
  }

  function scheduleSave() {
    if (saveTimer !== null) clearTimeoutImpl(saveTimer);
    saveTimer = setTimeoutImpl(() => {
      saveTimer = null;
      saveQueue = saveQueue.then(persist);
      return saveQueue;
    }, 250);
  }

  const client = {
    load,
    retry() {
      if (retryPromise) return retryPromise;
      const operation = failedOperation;
      if (!operation) return Promise.resolve(immutable(state));
      if (saveTimer !== null) {
        clearTimeoutImpl(saveTimer);
        saveTimer = null;
      }
      retryPromise = (operation === 'save'
        ? (saveQueue = saveQueue.then(persist))
        : (() => {
          loadPromise = null;
          return load();
        })())
        .finally(() => { retryPromise = null; });
      return retryPromise;
    },
    snapshot: () => immutable(state),
    settingsError: () => currentError && immutable(currentError),
    get(key, fallback = null) {
      const legacySchema = LEGACY_WIDTH_SCHEMAS[key];
      if (legacySchema) {
        try {
          const value = localStorage.getItem(key);
          const parsed = value === null ? fallback : JSON.parse(value);
          return legacySchema(parsed) ? parsed : fallback;
        } catch (_) {
          return fallback;
        }
      }
      return FIELD_SCHEMAS[key]?.(state[key]) ? clone(state[key]) : fallback;
    },
    set(key, value) {
      const legacySchema = LEGACY_WIDTH_SCHEMAS[key];
      if (legacySchema) {
        if (!legacySchema(value)) return false;
        try {
          localStorage.setItem(key, JSON.stringify(value));
          return true;
        } catch (_) {
          return false;
        }
      }
      if (!FIELD_SCHEMAS[key]?.(value)) return false;
      overrides[key] = clone(value);
      state = immutable({ ...state, [key]: clone(value) });
      scheduleSave();
      return true;
    },
  };

  // Start exactly one background read. It is intentionally detached so a
  // preference-file failure cannot delay or reject vault initialization.
  void load();
  return Object.freeze(client);
}
