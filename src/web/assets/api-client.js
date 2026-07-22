export function createApiClient({ token, onInflight, fetchImpl = globalThis.fetch, xhrFactory = () => new XMLHttpRequest() }) {
  let inflight = 0;

  return async function api(method, path, body, raw = false) {
    inflight++;
    onInflight?.(inflight);
    try {
      const opts = { method, headers: { Authorization: `Bearer ${token}` } };
      if (body instanceof FormData) {
        opts.body = body;
      } else if (body !== undefined) {
        opts.headers['Content-Type'] = 'application/json';
        opts.body = JSON.stringify(body);
      }
      const res = await fetchImpl(path, opts);
      if (!res.ok) {
        let msg = res.statusText;
        try { msg = (await res.json()).error || msg; } catch { /* not json */ }
        const error = new Error(msg);
        error.status = res.status;
        throw error;
      }
      if (raw) return res;
      const text = await res.text();
      return text ? JSON.parse(text) : null;
    } finally {
      inflight--;
      onInflight?.(inflight);
    }
  };
}

export function createXhr(xhrFactory = () => new XMLHttpRequest()) {
  return xhrFactory();
}
