const PREFERENCE_SCHEMAS = {
  'xv.ui.columns.secrets.v1': (value) => Array.isArray(value)
    && value.length === 5
    && value.every((width) => Number.isFinite(width)),
  'xv.ui.columns.files.v1': (value) => Array.isArray(value)
    && value.length === 4
    && value.every((width) => Number.isFinite(width)),
};

export function createPreferenceClient(api) {
  return {
    get(key, fallback = null) {
      const isValid = PREFERENCE_SCHEMAS[key];
      if (!isValid) return fallback;
      try {
        const value = localStorage.getItem(key);
        const parsed = value === null ? fallback : JSON.parse(value);
        return isValid(parsed) ? parsed : fallback;
      } catch (_) {
        return fallback;
      }
    },
    set(key, value) {
      if (!PREFERENCE_SCHEMAS[key]?.(value)) return false;
      try {
        localStorage.setItem(key, JSON.stringify(value));
        return true;
      } catch (_) {
        return false;
      }
    },
    api,
  };
}
