import { strict as assert } from 'node:assert';
import test from 'node:test';

import {
  connectionBanner,
  createConnectionMonitor,
  mountConnectionMonitor,
  probeOnce,
} from './connection.js';
import { createStore, draftReducer } from './store.js';

function fakeTimers() {
  let nextId = 1;
  const timeouts = new Map();
  return {
    setTimeout(fn, delay) { timeouts.set(nextId, { fn, delay }); return nextId++; },
    clearTimeout(id) { timeouts.delete(id); },
    fireTimeouts() { for (const { fn } of [...timeouts.values()]) fn(); },
    pending: () => [...timeouts.values()],
    pendingCount: () => timeouts.size,
    delays: () => [...timeouts.values()].map(({ delay }) => delay),
  };
}

function fakeDocument({ visibilityState = 'visible', elements = {} } = {}) {
  const listeners = new Map();
  return {
    visibilityState,
    addEventListener(type, fn) { listeners.set(type, fn); },
    removeEventListener(type) { listeners.delete(type); },
    getElementById: (id) => elements[id] || null,
    emit(type) { listeners.get(type)?.(); },
    listenerCount: () => listeners.size,
  };
}

const element = () => ({ hidden: true, textContent: '' });

test('banner copy names the fix for each state', () => {
  assert.equal(connectionBanner('ok').hidden, true);
  const dead = connectionBanner('disconnected');
  assert.equal(dead.hidden, false);
  assert.match(dead.message, /restart it with xv ui/);
  const stale = connectionBanner('session-expired');
  assert.equal(stale.hidden, false);
  assert.match(stale.message, /Reopen the URL/);
});

test('probeOnce sends the bearer token and reads liveness from the response', async () => {
  const seen = [];
  const status = await probeOnce({
    token: 'sekrit',
    timers: fakeTimers(),
    fetchImpl: async (path, opts) => { seen.push([path, opts.headers.Authorization]); return { status: 200 }; },
  });
  assert.equal(status, 'ok');
  assert.deepEqual(seen, [['/api/health', 'Bearer sekrit']]);
});

test('probeOnce treats 401 as a live server with a new session, not a dead one', async () => {
  const status = await probeOnce({
    token: 'stale',
    timers: fakeTimers(),
    fetchImpl: async () => ({ status: 401 }),
  });
  assert.equal(status, 'session-expired');
});

test('probeOnce reports a rejected fetch as disconnected and clears its timeout', async () => {
  const timers = fakeTimers();
  const status = await probeOnce({
    token: 't',
    timers,
    fetchImpl: async () => { throw new TypeError('Failed to fetch'); },
  });
  assert.equal(status, 'disconnected');
  assert.equal(timers.pendingCount(), 0);
});

test('probeOnce aborts a hung server via the timeout', async () => {
  const timers = fakeTimers();
  const status = await probeOnce({
    token: 't',
    timers,
    fetchImpl: (_path, opts) => new Promise((_resolve, reject) => {
      opts.signal.addEventListener('abort', () => reject(new Error('aborted')));
      timers.fireTimeouts();
    }),
  });
  assert.equal(status, 'disconnected');
});

test('a single failed probe does not raise the banner; a second one does', async () => {
  const timers = fakeTimers();
  const states = [];
  let result = 'disconnected';
  const monitor = createConnectionMonitor({
    probe: async () => result,
    onChange: (state) => states.push(state),
    timers,
    document: fakeDocument(),
  });
  monitor.start();
  await monitor.check();
  assert.deepEqual(states, []);
  await monitor.check();
  assert.deepEqual(states, ['disconnected']);

  result = 'ok';
  await monitor.check();
  assert.deepEqual(states, ['disconnected', 'ok']);
  monitor.stop();
});

test('failure count resets on success so flaps do not accumulate', async () => {
  const results = ['disconnected', 'ok', 'disconnected'];
  const states = [];
  const monitor = createConnectionMonitor({
    probe: async () => results.shift(),
    onChange: (state) => states.push(state),
    timers: fakeTimers(),
    document: fakeDocument(),
  });
  monitor.start();
  for (let i = 0; i < 3; i++) await monitor.check();
  assert.deepEqual(states, []);
  monitor.stop();
});

test('a session-expired probe is terminal and stops polling', async () => {
  const timers = fakeTimers();
  const states = [];
  const monitor = createConnectionMonitor({
    probe: async () => 'session-expired',
    onChange: (state) => states.push(state),
    timers,
    document: fakeDocument(),
  });
  monitor.start();
  await monitor.check();
  assert.deepEqual(states, ['session-expired']);
  assert.equal(timers.pendingCount(), 0);
  monitor.stop();
});

test('an unconfirmed failure reschedules fast; settled states wait the interval', async () => {
  const timers = fakeTimers();
  let result = 'ok';
  const monitor = createConnectionMonitor({
    probe: async () => result,
    intervalMs: 10_000,
    confirmDelayMs: 1_000,
    timers,
    document: fakeDocument(),
  });
  monitor.start();
  await monitor.check();
  assert.deepEqual(timers.delays(), [10_000]);

  result = 'disconnected';
  await monitor.check();
  assert.deepEqual(timers.delays(), [1_000], 'first failure rechecks quickly');

  await monitor.check();
  assert.equal(monitor.state(), 'disconnected');
  assert.deepEqual(timers.delays(), [10_000], 'a confirmed outage backs off again');
  monitor.stop();
});

test('polling continues after a confirmed outage so recovery is noticed', async () => {
  const timers = fakeTimers();
  const states = [];
  let result = 'disconnected';
  const monitor = createConnectionMonitor({
    probe: async () => result,
    onChange: (state) => states.push(state),
    timers,
    document: fakeDocument(),
  });
  monitor.start();
  await monitor.check();
  await monitor.check();
  assert.deepEqual(states, ['disconnected']);
  assert.equal(timers.pendingCount(), 1, 'still scheduled while disconnected');

  result = 'ok';
  timers.fireTimeouts();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(states, ['disconnected', 'ok']);
  monitor.stop();
});

test('a hidden tab does not poll and probes immediately when it returns', async () => {
  const timers = fakeTimers();
  const doc = fakeDocument({ visibilityState: 'hidden' });
  let probes = 0;
  const monitor = createConnectionMonitor({
    probe: async () => { probes++; return 'ok'; },
    timers,
    document: doc,
  });
  monitor.start();
  assert.equal(timers.pendingCount(), 0);
  assert.equal(probes, 0);

  doc.visibilityState = 'visible';
  doc.emit('visibilitychange');
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(probes, 1);
  assert.equal(timers.pendingCount(), 1);

  doc.visibilityState = 'hidden';
  doc.emit('visibilitychange');
  assert.equal(timers.pendingCount(), 0);
  monitor.stop();
});

test('overlapping checks collapse into one in-flight probe', async () => {
  let probes = 0;
  let release;
  const monitor = createConnectionMonitor({
    probe: () => { probes++; return new Promise((resolve) => { release = () => resolve('ok'); }); },
    timers: fakeTimers(),
    document: fakeDocument(),
  });
  monitor.start();
  const first = monitor.check();
  const second = monitor.check();
  release();
  await Promise.all([first, second]);
  assert.equal(probes, 1);
  monitor.stop();
});

test('stop clears the pending probe and the visibility listener', async () => {
  const timers = fakeTimers();
  const doc = fakeDocument();
  const monitor = createConnectionMonitor({
    probe: async () => 'ok',
    timers,
    document: doc,
  });
  monitor.start();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(timers.pendingCount(), 1);
  assert.equal(doc.listenerCount(), 1);
  monitor.stop();
  assert.equal(timers.pendingCount(), 0);
  assert.equal(doc.listenerCount(), 0);
});

test('the mounted monitor drives the store and the banner element', async () => {
  const timers = fakeTimers();
  const elements = {
    'connection-banner': element(),
    'connection-banner-title': element(),
    'connection-banner-message': element(),
  };
  const doc = fakeDocument({ elements });
  const store = createStore({ connection: 'ok' }, draftReducer);
  const mounted = mountConnectionMonitor({
    store,
    token: 't',
    document: doc,
    timers,
    fetchImpl: async () => { throw new TypeError('Failed to fetch'); },
  });

  await mounted.check();
  await mounted.check();
  assert.equal(store.snapshot().connection, 'disconnected');
  assert.equal(elements['connection-banner'].hidden, false);
  assert.equal(elements['connection-banner-title'].textContent, 'Disconnected');
  assert.match(elements['connection-banner-message'].textContent, /xv ui/);
  mounted.stop();
});

test('the reducer ignores unknown connection states', () => {
  const store = createStore({ connection: 'ok' }, draftReducer);
  store.dispatch({ type: 'connection/state', state: 'sideways' });
  assert.equal(store.snapshot().connection, 'ok');
  store.dispatch({ type: 'connection/state', state: 'disconnected' });
  assert.equal(store.snapshot().connection, 'disconnected');
});
