// Detects that the `xv ui` process the tab is talking to has gone away.
//
// The UI is otherwise entirely demand-driven: nothing tells the browser the
// server died until the user clicks something and the request fails. This
// polls a backend-free liveness route so a stale tab announces itself instead
// of looking healthy right up to the next failed action.

export const PROBE_INTERVAL_MS = 10_000;
export const PROBE_TIMEOUT_MS = 5_000;
// After a failure, confirm quickly instead of waiting out a full interval —
// the user is most likely mid-action when the server dies.
export const CONFIRM_DELAY_MS = 1_000;
// One failed probe against loopback is nearly conclusive, but a machine
// resuming from sleep can produce a single bogus rejection. Two consecutive
// failures keeps that from flashing a scary banner at wake-up.
export const FAILURES_BEFORE_DISCONNECTED = 2;

export const CONNECTION_STATES = Object.freeze(['ok', 'disconnected', 'session-expired']);

/** Banner copy for a connection state. Pure so it can be asserted directly. */
export function connectionBanner(state) {
  switch (state) {
    case 'disconnected':
      return {
        hidden: false,
        title: 'Disconnected',
        message: 'The xv ui server is no longer running. Nothing on this page can be saved or refreshed until you restart it with xv ui in your terminal.',
      };
    case 'session-expired':
      return {
        hidden: false,
        title: 'Session link required',
        message: 'xv ui restarted with a new session link. Reopen the URL printed in your terminal to continue.',
      };
    default:
      return { hidden: true, title: '', message: '' };
  }
}

/**
 * Classify one probe attempt.
 *
 * A 401 means the socket answered, so the process is alive — it is a *new*
 * process with a new token, which is a different problem with a different fix
 * than a dead server. Any other HTTP status still proves liveness.
 */
export async function probeOnce({ fetchImpl, token, timeoutMs = PROBE_TIMEOUT_MS, timers = globalThis }) {
  const controller = new AbortController();
  const timer = timers.setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetchImpl('/api/health', {
      method: 'GET',
      headers: { Authorization: `Bearer ${token}` },
      signal: controller.signal,
    });
    return res.status === 401 ? 'session-expired' : 'ok';
  } catch {
    return 'disconnected';
  } finally {
    timers.clearTimeout(timer);
  }
}

/**
 * Poll `probe` while the tab is visible and report state transitions.
 *
 * Polling pauses when the tab is hidden — a backgrounded tab that cannot act
 * on the news does not need to keep asking — and probes immediately on the
 * way back so the banner is already correct when the user returns.
 */
export function createConnectionMonitor({
  probe,
  onChange,
  intervalMs = PROBE_INTERVAL_MS,
  confirmDelayMs = CONFIRM_DELAY_MS,
  failuresBeforeDisconnected = FAILURES_BEFORE_DISCONNECTED,
  timers = globalThis,
  document: doc = globalThis.document,
}) {
  let state = 'ok';
  let failures = 0;
  let timer = null;
  let running = false;
  let inflight = null;

  const visible = () => doc?.visibilityState !== 'hidden';

  function transition(next) {
    if (next === state) return;
    state = next;
    onChange?.(state);
  }

  function stopTimer() {
    if (timer !== null) {
      timers.clearTimeout(timer);
      timer = null;
    }
  }

  function schedule() {
    stopTimer();
    if (!running || !visible() || state === 'session-expired') return;
    // An unconfirmed failure gets a fast recheck; a settled state waits out
    // the full interval.
    const delay = failures > 0 && state !== 'disconnected' ? confirmDelayMs : intervalMs;
    timer = timers.setTimeout(() => { timer = null; void tick(); }, delay);
  }

  async function tick() {
    if (inflight) return inflight;
    inflight = (async () => {
      const result = await probe();
      if (!running) return;
      if (result === 'ok') {
        failures = 0;
        transition('ok');
      } else if (result === 'session-expired') {
        // Terminal: this tab cannot learn the new token, so stop probing.
        failures = 0;
        transition('session-expired');
      } else {
        failures++;
        if (failures >= failuresBeforeDisconnected) transition('disconnected');
      }
      schedule();
    })().finally(() => { inflight = null; });
    return inflight;
  }

  function onVisibilityChange() {
    if (!running) return;
    if (visible()) void tick();
    else stopTimer();
  }

  return {
    start() {
      if (running) return;
      running = true;
      doc?.addEventListener?.('visibilitychange', onVisibilityChange);
      if (visible()) void tick();
    },
    stop() {
      running = false;
      stopTimer();
      doc?.removeEventListener?.('visibilitychange', onVisibilityChange);
    },
    /** Probe now — used when a real request just failed at the transport. */
    check: () => tick(),
    state: () => state,
  };
}

/** Wire a monitor to the store and the banner element. */
export function mountConnectionMonitor({
  store,
  token,
  fetchImpl = globalThis.fetch,
  document: doc = globalThis.document,
  timers = globalThis,
  intervalMs = PROBE_INTERVAL_MS,
}) {
  const monitor = createConnectionMonitor({
    probe: () => probeOnce({ fetchImpl, token, timers }),
    onChange: (state) => store.dispatch({ type: 'connection/state', state }),
    intervalMs,
    timers,
    document: doc,
  });

  const banner = doc.getElementById('connection-banner');
  const title = doc.getElementById('connection-banner-title');
  const message = doc.getElementById('connection-banner-message');

  // The banner is fixed to the viewport so it cannot be scrolled away from,
  // which means the layout has to be told how much room it takes. Measure it
  // rather than hard-coding a height: the copy wraps to two or three lines on
  // narrow viewports, and a stale guess would either cover the header or
  // leave a gap.
  const syncHeight = () => {
    const height = banner && !banner.hidden ? banner.offsetHeight || 0 : 0;
    doc.documentElement?.style?.setProperty('--connection-banner-height', `${height}px`);
  };
  const observer = banner && typeof globalThis.ResizeObserver === 'function'
    ? new globalThis.ResizeObserver(syncHeight)
    : null;
  observer?.observe(banner);

  const unsubscribe = store.subscribe((snapshot) => {
    if (!banner) return;
    const copy = connectionBanner(snapshot.connection);
    banner.hidden = copy.hidden;
    if (title) title.textContent = copy.title;
    if (message) message.textContent = copy.message;
    syncHeight();
  });

  monitor.start();
  return {
    check: monitor.check,
    state: monitor.state,
    stop() {
      unsubscribe();
      observer?.disconnect();
      monitor.stop();
    },
  };
}
