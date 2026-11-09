import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule } from 'node:vm';

const source = stripTypeScriptTypes(
  await readFile(new URL('../src/updates.ts', import.meta.url), 'utf8'),
);
const HOUR = 60 * 60 * 1000;

function eventTarget() {
  const listeners = new Map();
  return {
    addEventListener(event, callback) {
      if (!listeners.has(event)) listeners.set(event, new Set());
      listeners.get(event).add(callback);
    },
    removeEventListener(event, callback) { listeners.get(event)?.delete(callback); },
    emit(event) { for (const callback of listeners.get(event) ?? []) callback(); },
    listenerCount() { return [...listeners.values()].reduce((sum, set) => sum + set.size, 0); },
  };
}

async function setup(fetchUpdate = async () => null) {
  let now = 0;
  let nextTimer = 0;
  const timers = new Map();
  const requests = [];
  const notifications = [];
  const setTimer = (callback, delay, interval) => {
    const id = ++nextTimer;
    timers.set(id, { callback, at: now + delay, interval });
    return id;
  };
  const window = {
    ...eventTarget(),
    setTimeout: (callback, delay) => setTimer(callback, delay, null),
    setInterval: (callback, delay) => setTimer(callback, delay, delay),
    clearTimeout: (id) => timers.delete(id),
    clearInterval: (id) => timers.delete(id),
  };
  const document = { ...eventTarget(), visibilityState: 'visible' };
  const context = createContext({ window, document, Date: { now: () => now } });
  const module = new SourceTextModule(source, { context });
  await module.link(() => { throw new Error('Unexpected runtime import'); });
  await module.evaluate();
  const checker = module.namespace.createUpdateChecker(() => {
    requests.push(now);
    return fetchUpdate();
  }, (info) => notifications.push(info));
  checker.start();
  const flush = async () => {
    for (let i = 0; i < 10; i++) await Promise.resolve();
  };
  return {
    checker, window, document, timers, requests, notifications, flush,
    jump: (elapsed) => { now += elapsed; },
    async advance(elapsed) {
      const end = now + elapsed;
      while (true) {
        const next = [...timers.entries()]
          .filter(([, timer]) => timer.at <= end)
          .sort((a, b) => a[1].at - b[1].at)[0];
        if (!next) break;
        const [id, timer] = next;
        now = Math.max(now, timer.at);
        if (timer.interval === null) timers.delete(id);
        else timer.at = now + timer.interval;
        timer.callback();
        await flush();
      }
      now = end;
      await flush();
    },
  };
}

function release(version = '1.3.1') {
  return {
    currentVersion: '1.3.0', latestVersion: version,
    name: `YiTerm ${version}`, notes: 'Release notes',
    assetName: `YiTerm_${version}_aarch64.dmg`,
    assetUrl: `https://github.com/Yukuiii/YiTerm/releases/download/v${version}/YiTerm_${version}_aarch64.dmg`,
    publishedAt: null,
  };
}

test('checks after the startup delay and every hour, including the first hourly tick', async (t) => {
  const env = await setup();
  t.after(env.checker.stop);
  await env.advance(2499);
  assert.equal(env.requests.length, 0);
  await env.advance(1);
  assert.deepEqual(env.requests, [2500]);
  await env.advance(HOUR - 2500);
  assert.deepEqual(env.requests, [2500, HOUR]);
  await env.advance(HOUR);
  assert.deepEqual(env.requests, [2500, HOUR, 2 * HOUR]);
  assert.deepEqual(env.notifications, []);
});

test('a manual check before startup avoids a redundant startup request', async (t) => {
  const env = await setup();
  t.after(env.checker.stop);
  assert.equal(await env.checker.check(true), null);
  await env.advance(2500);
  assert.equal(env.requests.length, 1);
  await env.advance(HOUR - 2500);
  assert.equal(env.requests.length, 2);
});

test('automatic and manual checks share one in-flight request', async (t) => {
  const pending = Promise.withResolvers();
  const info = release();
  const env = await setup(() => pending.promise);
  t.after(env.checker.stop);
  await env.advance(2500);
  const manual = env.checker.check(true);
  assert.equal(env.checker.check(), manual);
  assert.equal(env.requests.length, 1);
  pending.resolve(info);
  assert.equal(await manual, info);
  assert.deepEqual(env.notifications, [info]);
});

test('focus and visibility check only when overdue and coalesce on return', async (t) => {
  const env = await setup();
  t.after(env.checker.stop);
  env.window.emit('focus');
  env.document.emit('visibilitychange');
  await env.flush();
  assert.equal(env.requests.length, 0, 'focus must preserve the startup delay');
  await env.advance(2500);
  env.window.emit('focus');
  await env.flush();
  assert.equal(env.requests.length, 1);
  env.jump(HOUR);
  env.window.emit('focus');
  await env.flush();
  assert.equal(env.requests.length, 2);
  env.jump(HOUR);
  env.document.visibilityState = 'hidden';
  env.document.emit('visibilitychange');
  env.window.emit('focus');
  await env.flush();
  assert.equal(env.requests.length, 2);
  env.document.visibilityState = 'visible';
  env.document.emit('visibilitychange');
  env.window.emit('focus');
  await env.flush();
  assert.equal(env.requests.length, 3);
});

test('dismissed releases stay quiet automatically but can be reopened manually', async (t) => {
  let available = release();
  const env = await setup(async () => available);
  t.after(env.checker.stop);
  await env.advance(2500);
  env.checker.dismiss();
  await env.advance(HOUR - 2500);
  assert.deepEqual(env.notifications, [available, null]);
  await env.checker.check(true);
  assert.deepEqual(env.notifications, [available, null, available]);
  env.checker.dismiss();
  available = release('1.4.0');
  await env.advance(HOUR);
  assert.deepEqual(env.notifications.filter(Boolean).map((info) => info.latestVersion), ['1.3.1', '1.3.1', '1.4.0']);
});

test('a manual caller joining a background recheck can reopen a dismissed release', async (t) => {
  const info = release();
  const pending = Promise.withResolvers();
  let attempts = 0;
  const env = await setup(() => ++attempts === 1 ? Promise.resolve(info) : pending.promise);
  t.after(env.checker.stop);
  await env.advance(2500);
  env.checker.dismiss();
  const automatic = env.checker.check();
  const manual = env.checker.check(true);
  assert.equal(manual, automatic);
  pending.resolve(info);
  await manual;
  assert.deepEqual(env.notifications, [info, null, info]);
  assert.equal(env.requests.length, 2);
});

test('an active dialog keeps its release and does not start more checks', async (t) => {
  let available = release();
  const env = await setup(async () => available);
  t.after(env.checker.stop);
  const original = await env.checker.check(true);
  available = release('1.4.0');
  await env.advance(HOUR);
  assert.equal(await env.checker.check(true), original);
  assert.equal(env.requests.length, 1);
  assert.deepEqual(env.notifications, [original]);
  env.checker.dismiss();
  await env.checker.check();
  assert.deepEqual(env.notifications, [original, null, available]);
});

test('failed automatic checks are quiet, manual failures reject, and retries recover', async (t) => {
  let fail = true;
  const env = await setup(async () => {
    if (fail) throw new Error('Network unavailable');
    return null;
  });
  t.after(env.checker.stop);
  await env.advance(2500);
  assert.deepEqual(env.notifications, []);
  await assert.rejects(env.checker.check(true), /Network unavailable/);
  fail = false;
  assert.equal(await env.checker.check(true), null);
  await env.advance(HOUR - 2500);
  assert.equal(env.requests.length, 4);
});

test('stop cleans up timers and listeners, and start does not duplicate them', async () => {
  const env = await setup();
  env.checker.start();
  assert.equal(env.timers.size, 2);
  assert.equal(env.window.listenerCount(), 1);
  assert.equal(env.document.listenerCount(), 1);
  env.checker.stop();
  assert.equal(env.timers.size, 0);
  assert.equal(env.window.listenerCount(), 0);
  assert.equal(env.document.listenerCount(), 0);
  await env.advance(2 * HOUR);
  env.window.emit('focus');
  await env.flush();
  assert.equal(env.requests.length, 0);
  await assert.rejects(env.checker.check(true), /更新检查已停止/);
});

test('a stale completion cannot show a dialog or clear a newer request after restart', async (t) => {
  const old = Promise.withResolvers();
  const current = Promise.withResolvers();
  let attempts = 0;
  const env = await setup(() => ++attempts === 1 ? old.promise : current.promise);
  t.after(env.checker.stop);
  const oldRequest = env.checker.check(true);
  await env.flush();
  env.checker.stop();
  env.checker.start();
  const currentRequest = env.checker.check(true);
  await env.flush();
  old.resolve(release());
  await oldRequest;
  assert.deepEqual(env.notifications, []);
  assert.equal(env.checker.check(true), currentRequest);
  assert.equal(env.requests.length, 2);
  const info = release('1.4.0');
  current.resolve(info);
  await currentRequest;
  assert.deepEqual(env.notifications, [info]);
});
