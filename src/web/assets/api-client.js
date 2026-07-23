export class ApiError extends Error {
  constructor({ status, code = 'xv-request-failed', message = 'The request could not be completed.', hint = null, field = null, details = null }) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.code = code;
    this.hint = hint;
    this.field = field;
    this.details = details;
  }
}

export function createApiClient({ token, onInflight, fetchImpl = globalThis.fetch, xhrFactory = () => new XMLHttpRequest() }) {
  let inflight = 0;

  const api = async function api(method, path, body, raw = false, requestOptions = {}) {
    inflight++;
    onInflight?.(inflight);
    try {
      const opts = {
        method,
        headers: { Authorization: `Bearer ${token}` },
        signal: requestOptions.signal,
      };
      if (body instanceof FormData) {
        opts.body = body;
      } else if (body !== undefined) {
        opts.headers['Content-Type'] = 'application/json';
        opts.body = JSON.stringify(body);
      }
      const res = await fetchImpl(path, opts);
      if (!res.ok) {
        let envelope = null;
        try { envelope = await res.json(); } catch { /* non-JSON failure */ }
        const body = envelope?.error && typeof envelope.error === 'object' ? envelope.error : {};
        throw new ApiError({
          status: res.status,
          code: body.code,
          message: body.message,
          hint: body.hint,
          field: body.field,
          details: body.details,
        });
      }
      if (raw) return res;
      const text = await res.text();
      return text ? JSON.parse(text) : null;
    } finally {
      inflight--;
      onInflight?.(inflight);
    }
  };
  api.createXhr = () => xhrFactory();
  api.request = api;
  return api;
}
