import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule } from 'node:vm';

const source = stripTypeScriptTypes(await readFile(new URL('../src/planApproval.ts', import.meta.url), 'utf8'));
const ready = (token = 42) => ({ available: true, token, modes: ['manual', 'bypass'] });

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

async function setup(fetch = async () => ready(), send = async () => {}) {
  let nextTimer = 0;
  const timers = new Map();
  const states = [];
  const requests = [];
  const decisions = [];
  const window = {
    ...eventTarget(),
    setInterval(callback, delay) {
      assert.equal(delay, 500);
      const id = ++nextTimer;
      timers.set(id, callback);
      return id;
    },
    clearInterval(id) { timers.delete(id); },
  };
  const document = { ...eventTarget(), visibilityState: 'visible' };
  const module = new SourceTextModule(source, { context: createContext({ window, document }) });
  await module.link(() => { throw new Error('Unexpected runtime import'); });
  await module.evaluate();
  const controller = module.namespace.createPlanApprovalController(
    () => { requests.push({}); return fetch(); },
    (token, mode) => { decisions.push({ token, mode }); return send(token, mode); },
    (state) => states.push(state),
  );
  const flush = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };
  controller.start();
  await flush();
  return { controller, window, document, timers, states, requests, decisions, flush, latestPlanCall: module.namespace.latestPlanCall };
}

test('the newest plan settles eligibility even when an older plan remains unresolved', async (t) => {
  const env = await setup();
  t.after(env.controller.stop);
  const message = (key, id, result) => ({
    key, role: 'assistant', toolCalls: [{ id, name: 'ExitPlanMode', ...(result !== undefined ? { result } : {}) }],
  });
  const old = message('old', 'old-call');
  const latest = message('new', 'new-call', '');
  const found = env.latestPlanCall([old, latest, { role: 'user', key: 'user', toolCalls: [] }]);
  assert.equal(found.callId, 'new-call');
  assert.equal(found.resolved, true, 'even an empty tool result resolves the specific call');
  assert.equal(env.latestPlanCall([old]).resolved, false);
  assert.equal(env.latestPlanCall([]), null);
});

test('polling coalesces requests, pauses while hidden, refreshes on return, and cleans up', async (t) => {
  const pending = Promise.withResolvers();
  const env = await setup(() => pending.promise);
  t.after(env.controller.stop);
  for (const callback of env.timers.values()) { callback(); callback(); }
  env.window.emit('focus');
  assert.equal(env.requests.length, 1);
  pending.resolve(ready());
  await env.flush();
  env.document.visibilityState = 'hidden';
  env.document.emit('visibilitychange');
  env.window.emit('focus');
  for (const callback of env.timers.values()) callback();
  await env.flush();
  assert.equal(env.requests.length, 1);
  env.document.visibilityState = 'visible';
  env.document.emit('visibilitychange');
  await env.flush();
  assert.equal(env.requests.length, 2);
  env.controller.stop();
  assert.equal(env.timers.size, 0);
  assert.equal(env.window.listenerCount(), 0);
  assert.equal(env.document.listenerCount(), 0);
});

test('a decision uses the observed token once and ignores an older in-flight status response', async (t) => {
  const query = Promise.withResolvers();
  const send = Promise.withResolvers();
  let attempts = 0;
  const env = await setup(() => ++attempts === 1 ? Promise.resolve(ready()) : query.promise, () => send.promise);
  t.after(env.controller.stop);
  const refresh = env.controller.refresh();
  const decision = env.controller.decide('bypass');
  assert.equal(env.states.at(-1).submitting, true);
  assert.equal(await env.controller.decide('manual'), false);
  assert.deepEqual(env.decisions, [{ token: 42, mode: 'bypass' }]);
  query.resolve(ready(99));
  await refresh;
  assert.equal(env.states.at(-1).status.token, 42);
  assert.equal(env.states.at(-1).submitting, true);
  send.resolve();
  assert.equal(await decision, true);
  assert.equal(env.states.at(-1).sent, true);
  assert.equal(env.states.at(-1).status, null, 'a write receipt must not re-enable approval');
  for (const callback of env.timers.values()) callback();
  assert.equal(env.requests.length, 2);
  assert.equal(await env.controller.decide('manual'), false);
  assert.equal(env.decisions.length, 1);
});

test('failed decisions are never automatically retried and can recover after explicit retry', async (t) => {
  let attempts = 0;
  const env = await setup(async () => ready(), async () => {
    if (++attempts === 1) throw new Error('Prompt changed');
  });
  t.after(env.controller.stop);
  assert.equal(await env.controller.decide('manual'), false);
  await env.flush();
  assert.equal(env.states.at(-1).error, '执行选择未能确认，请在终端查看当前状态。');
  for (const callback of env.timers.values()) callback();
  await env.flush();
  assert.equal(env.decisions.length, 1);
  assert.equal(env.states.at(-1).error, '执行选择未能确认，请在终端查看当前状态。', 'polling must preserve the Chinese message without leaking the English error');
  assert.equal(await env.controller.decide('manual'), true);
  assert.equal(env.decisions.length, 2);
  assert.equal(env.states.at(-1).error, null);
});

test('stop and restart ignore old queries without clearing the current request', async (t) => {
  const old = Promise.withResolvers();
  const current = Promise.withResolvers();
  let attempts = 0;
  const env = await setup(() => ++attempts === 1 ? old.promise : current.promise);
  t.after(env.controller.stop);
  env.controller.stop();
  env.controller.start();
  old.resolve(ready(1));
  await env.flush();
  assert.equal(env.states.at(-1).status, null);
  assert.equal(env.states.at(-1).checking, true);
  void env.controller.refresh();
  assert.equal(env.requests.length, 2);
  current.resolve(ready(73));
  await env.flush();
  assert.equal(env.states.at(-1).status.token, 73);
});

test('unavailable or unsupported choices send nothing and query errors recover', async (t) => {
  let fail = true;
  const env = await setup(async () => {
    if (fail) throw new Error('Terminal unavailable');
    return { available: false, token: null, modes: [] };
  });
  t.after(env.controller.stop);
  assert.equal(env.states.at(-1).error, '暂时无法确认计划状态，请在终端继续。');
  assert.equal(await env.controller.decide('manual'), false);
  fail = false;
  await env.controller.refresh();
  assert.equal(env.states.at(-1).error, null);
  assert.equal(await env.controller.decide('bypass'), false);
  env.controller.stop();
  assert.equal(await env.controller.decide('manual'), false);
  assert.deepEqual(env.decisions, []);
});

test('a stopped plan context cannot publish a late decision completion', async () => {
  const pending = Promise.withResolvers();
  const env = await setup(async () => ready(), () => pending.promise);
  const decision = env.controller.decide('manual');
  env.controller.stop();
  const count = env.states.length;
  pending.resolve();
  await decision;
  assert.equal(env.states.length, count);
});
