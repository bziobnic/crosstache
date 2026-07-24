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

export function createApiClient({
  token,
  onInflight,
  onOperation,
  fetchImpl = globalThis.fetch,
  xhrFactory = () => new XMLHttpRequest(),
}) {
  let inflight = 0;
  let nextOperationId = 0;

  const api = async function api(method, path, body, raw = false, requestOptions = {}) {
    const operationId = requestOptions.operationId || `request-${++nextOperationId}`;
    inflight++;
    onInflight?.(inflight);
    onOperation?.({ operationId, status: 'started' });
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
      if (raw) {
        onOperation?.({ operationId, status: 'succeeded' });
        return res;
      }
      const text = await res.text();
      const result = text ? JSON.parse(text) : null;
      onOperation?.({ operationId, status: 'succeeded' });
      return result;
    } catch (error) {
      onOperation?.({
        operationId,
        status: error?.name === 'AbortError' ? 'cancelled' : 'failed',
      });
      throw error;
    } finally {
      inflight--;
      onInflight?.(inflight);
    }
  };
  api.createXhr = () => xhrFactory();
  api.upload = ({ path, formData, signal, onProgress, operationId: suppliedOperationId }) => {
    const operationId = suppliedOperationId || `request-${++nextOperationId}`;
    inflight++;
    onInflight?.(inflight);
    onOperation?.({ operationId, status: 'started' });
    return new Promise((resolve, reject) => {
      const xhr = xhrFactory();
      let settled = false;
      let finishing = false;
      let progress = { loaded: 0, total: Number(formData?.get?.('file')?.size) || 0 };
      const finish = (status, callback) => {
        if (settled) return;
        settled = true;
        signal?.removeEventListener?.('abort', abort);
        inflight--;
        onInflight?.(inflight);
        onOperation?.({ operationId, status });
        callback();
      };
      const abortError = () => Object.assign(new Error('Upload cancelled.'), { name: 'AbortError' });
      const abort = () => xhr.abort();
      if (signal?.aborted) {
        finish('cancelled', () => reject(abortError()));
        return;
      }
      xhr.open('POST', path, true);
      xhr.setRequestHeader('Authorization', `Bearer ${token}`);
      xhr.upload.onprogress = ({ loaded, total, lengthComputable }) => {
        progress = { loaded, total: lengthComputable === false ? progress.total : total };
        onProgress?.({ ...progress });
      };
      xhr.upload.onload = () => {
        finishing = true;
        progress = { loaded: progress.total, total: progress.total };
        onProgress?.({ ...progress, finishing: true });
      };
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          try {
            const result = xhr.responseText ? JSON.parse(xhr.responseText) : null;
            const confirmed = result && typeof result === 'object' && (
              (typeof result.name === 'string' && result.name.length > 0)
              || (result.status === 'skipped' && typeof result.name === 'string' && result.name.length > 0)
            );
            if (!confirmed) throw new TypeError('incomplete confirmation');
            finish('succeeded', () => resolve(result));
          } catch {
            const error = Object.assign(
              new Error('The server did not provide a valid upload confirmation.'),
              { name: 'AmbiguousUploadError', ambiguous: true },
            );
            finish('failed', () => reject(error));
          }
          return;
        }
        let envelope = null;
        try { envelope = JSON.parse(xhr.responseText); } catch { /* non-JSON failure */ }
        const body = envelope?.error && typeof envelope.error === 'object' ? envelope.error : {};
        finish('failed', () => reject(new ApiError({
          status: xhr.status,
          code: body.code,
          message: body.message,
          hint: body.hint,
          field: body.field,
          details: body.details,
        })));
      };
      xhr.onerror = () => {
        const error = Object.assign(new Error('The upload connection was interrupted.'), {
          name: 'NetworkError',
          ambiguous: finishing,
        });
        finish('failed', () => reject(error));
      };
      xhr.onabort = () => {
        const error = abortError();
        error.ambiguous = finishing;
        finish('cancelled', () => reject(error));
      };
      signal?.addEventListener?.('abort', abort, { once: true });
      xhr.send(formData);
    });
  };
  api.request = api;
  return api;
}
