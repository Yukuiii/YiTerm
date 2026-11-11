import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule, SyntheticModule } from 'node:vm';
import { create } from 'zustand';

const source = stripTypeScriptTypes(
  await readFile(new URL('../src/stores/chat.ts', import.meta.url), 'utf8'),
);

// Exercise the real store with isolated module state, controlled timers and
// mocked desktop IPC. No webview or additional test dependencies are needed.
async function setup({
  save = async () => {},
  load = async () => null,
  translate = async () => { throw new Error('Unexpected translation'); },
  mode = null,
} = {}) {
  const timers = new Map();
  const writes = [];
  const translations = [];
  const serializations = [];
  let nextTimer = 0;
  const context = createContext({
    console,
    crypto: { randomUUID },
    JSON: {
      parse: JSON.parse,
      stringify(value) {
        serializations.push(value);
        return JSON.stringify(value);
      },
    },
    window: {
      setTimeout(callback, delay) {
        const id = ++nextTimer;
        timers.set(id, { callback, delay });
        return id;
      },
      clearTimeout(id) {
        timers.delete(id);
      },
    },
  });
  const exportsByModule = {
    zustand: { create },
    '../ipc': {
      chatLoad: load,
      chatSave: (convId, json) => {
        writes.push({ convId, json });
        return save(convId, json);
      },
      debugLog: async () => {},
      hasCJK: () => false,
      translate: (direction, text, onEvent) => {
        translations.push({ direction, text });
        return translate(direction, text, onEvent);
      },
    },
    './settings': {
      useSettingsStore: { getState: () => ({ settings: mode ? { translation: { mode } } : null }) },
    },
  };
  const module = new SourceTextModule(source, { context });
  await module.link((specifier) => {
    const exports = exportsByModule[specifier];
    if (!exports) throw new Error(`Unexpected import: ${specifier}`);
    return new SyntheticModule(Object.keys(exports), function () {
      for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
    }, { context });
  });
  await module.evaluate();

  return {
    store: module.namespace.useChatStore,
    writes,
    translations,
    serializations,
    async flush() {
      for (const [id, { callback, delay }] of [...timers]) {
        assert.equal(delay, 500, 'only persistence should be scheduled');
        timers.delete(id);
        callback();
      }
      // Let a rejected IPC promise update the persistence retry state.
      await Promise.resolve();
    },
  };
}

test('a flush saves all dirty conversations and skips unchanged histories', async () => {
  const { store, writes, serializations, flush } = await setup();
  const key = store.getState().addPlain('active', 'hello');
  store.getState().addPlain('background', 'history');
  await flush();
  assert.deepEqual(writes.map((write) => write.convId), ['active', 'background']);
  assert.equal(serializations.length, 2);

  writes.length = 0;
  serializations.length = 0;
  const background = store.getState().byConv.background;
  store.getState().appendDelta('active', key, 'translation');
  await flush();
  assert.equal(store.getState().byConv.background, background);
  assert.equal(serializations.length, 1);
  assert.equal(serializations[0], store.getState().byConv.active);
  assert.deepEqual(writes.map((write) => write.convId), ['active']);
});

test('identical snapshots do not trigger another write', async () => {
  const { store, writes, flush } = await setup();
  const key = store.getState().addPlain('conv', 'hello');
  await flush();
  store.getState().setWorking('conv', key, false);
  await flush();
  assert.equal(writes.length, 1);
});

test('attachments save immediately and are not serialized again by the timer', async () => {
  const { store, writes, serializations, flush } = await setup();
  store.getState().addPlain('conv', 'attached', ['/image.png'], ['/file.txt']);
  assert.equal(writes.length, 1);
  const [message] = JSON.parse(writes[0].json);
  assert.deepEqual(message.images, ['/image.png']);
  assert.deepEqual(message.files, ['/file.txt']);
  await flush();
  assert.equal(writes.length, 1);
  assert.equal(serializations.length, 1);
});

test('a failed save retries when another conversation changes', async () => {
  let fail = true;
  const { store, writes, flush } = await setup({
    save: async () => {
      if (fail) {
        fail = false;
        throw new Error('Temporary write failure');
      }
    },
  });
  store.getState().addPlain('first', 'hello');
  await flush();
  store.getState().addPlain('second', 'another conversation');
  await flush();
  assert.deepEqual(writes.map((write) => write.convId), ['first', 'first', 'second']);
  assert.equal(writes[0].json, writes[1].json);
});

test('a stale failure cannot invalidate a newer successful snapshot', async () => {
  const pending = Promise.withResolvers();
  let attempts = 0;
  const { store, writes, flush } = await setup({
    save: () => ++attempts === 1 ? pending.promise : Promise.resolve(),
  });
  const key = store.getState().addPlain('conv', 'hello');
  await flush();
  store.getState().setWorking('conv', key, true);
  await flush();
  pending.reject(new Error('Old write failed late'));
  await Promise.resolve();
  store.getState().setWorking('conv', key, true);
  await flush();
  assert.equal(writes.length, 2);
  assert.equal(JSON.parse(writes[1].json)[0].working, true);
});

test('tool-call updates preserve results, incoming order and previous calls', async () => {
  const { store } = await setup();
  const assistant = {
    key: 'assistant', role: 'assistant', text: '', ts: null,
    thinking: null, done: false,
    toolCalls: [
      { id: 'a', name: 'Read', label: 'first', detail: '' },
      { id: 'b', name: 'Bash', label: 'second', detail: '' },
    ],
  };
  store.getState().upsertFromSession('conv', [assistant], false);
  store.getState().upsertFromSession('conv', [{
    ...assistant, key: 'result', role: 'user', toolCalls: [],
    toolResult: { callId: 'b', text: 'failed output', isError: true },
  }], false);
  store.getState().upsertFromSession('conv', [{
    ...assistant,
    toolCalls: [
      { id: 'b', name: 'Bash', label: 'updated', detail: 'command' },
      { id: 'c', name: 'Write', label: 'new', detail: '' },
    ],
  }], false);
  const calls = store.getState().byConv.conv[0].toolCalls;
  assert.deepEqual(Array.from(calls, (call) => call.id), ['b', 'c', 'a']);
  assert.equal(calls[0].label, 'updated');
  assert.equal(calls[0].result, 'failed output');
  assert.equal(calls[0].isError, true);
});

const PLAN = '# Implementation plan\n\n1. Preserve history.\n2. Render Markdown.\n\n```ts\nconst ready = true;\n```\n\nFinal verification step without a trailing period';
const PLAN_CALL = { id: 'exit-plan', name: 'ExitPlanMode', label: 'ExitPlanMode', detail: 'plan.md' };

function planUpdate(text = PLAN) {
  return {
    key: 'plan', role: 'assistant', text, ts: null, thinking: null,
    done: true, toolCalls: [PLAN_CALL], toolResult: null,
  };
}

function cachedPlan(overrides = {}) {
  return {
    key: 'plan', role: 'assistant', original: '', translation: null,
    working: false, error: null, ts: null, thinking: null,
    toolCalls: [PLAN_CALL], done: true, ...overrides,
  };
}

async function settleTranslation() {
  for (let i = 0; i < 5; i++) await Promise.resolve();
}

test('plan replay repairs a cached bubble in place and survives approval and reopening', async () => {
  const cached = [
    cachedPlan({ key: 'before', role: 'user', original: 'Make a plan.', toolCalls: [] }),
    cachedPlan({ toolCalls: [PLAN_CALL, { id: 'read', name: 'Read', detail: 'file', result: 'retained output' }] }),
    cachedPlan({ key: 'after', original: 'Later response.', toolCalls: [] }),
  ];
  const { store, writes, translations, flush } = await setup({ load: async () => JSON.stringify(cached) });
  await store.getState().hydrate('conv');
  for (let i = 0; i < 3; i++) store.getState().upsertFromSession('conv', [planUpdate()], false);
  store.getState().upsertFromSession('conv', [{ ...planUpdate(''), toolCalls: [], done: false }], false);
  const messages = store.getState().byConv.conv;
  assert.deepEqual(Array.from(messages, (m) => m.key), ['before', 'plan', 'after']);
  assert.equal(messages[1].original, PLAN);
  assert.equal(messages[1].done, true);
  assert.equal(messages[1].toolCalls[0].result, undefined, 'plan display must not imply approval');
  assert.equal(messages[1].toolCalls[1].result, 'retained output');

  store.getState().upsertFromSession('conv', [{
    ...planUpdate(''), key: 'tr-exit-plan', role: 'user', toolCalls: [],
    toolResult: { callId: 'exit-plan', text: 'Plan approved.', isError: false },
  }], false);
  assert.equal(store.getState().byConv.conv.length, 3);
  assert.equal(store.getState().byConv.conv[1].original, PLAN);
  await flush();
  assert.equal(translations.length, 0);
  const restored = await setup({ load: async () => writes.at(-1).json });
  await restored.store.getState().hydrate('conv');
  const reopened = restored.store.getState().byConv.conv;
  assert.deepEqual(Array.from(reopened, (m) => m.key), ['before', 'plan', 'after']);
  assert.equal(reopened[1].original, PLAN);
  assert.equal(reopened[1].toolCalls[0].result, 'Plan approved.');
});

test('plan replay arriving during hydration wins over the older cached tool-only message', async () => {
  const pending = Promise.withResolvers();
  const { store } = await setup({ load: () => pending.promise });
  const hydration = store.getState().hydrate('conv');
  store.getState().upsertFromSession('conv', [planUpdate()], false);
  pending.resolve(JSON.stringify([cachedPlan()]));
  await hydration;
  assert.equal(store.getState().byConv.conv.length, 1);
  assert.equal(store.getState().byConv.conv[0].original, PLAN);
});

test('untranslated plan replay enriches source while retaining cached translation coverage', async () => {
  const preamble = 'Here is the plan.';
  const { store, translations } = await setup({
    load: async () => JSON.stringify([cachedPlan({
      original: preamble, translation: '以下是计划。', translatedFrom: preamble,
    })]),
  });
  await store.getState().hydrate('conv');
  const enriched = `${preamble}\n\n${PLAN}`;
  store.getState().upsertFromSession('conv', [planUpdate(enriched)], false);
  const message = store.getState().byConv.conv[0];
  assert.equal(message.original, enriched);
  assert.equal(message.translation, '以下是计划。');
  assert.equal(message.translatedFrom, preamble);
  assert.equal(translations.length, 0, 'imported historical replay must not translate');
});

test('native plan replay translates the full document before approval and skips unchanged replay', async () => {
  const pending = Promise.withResolvers();
  const { store, translations } = await setup({
    load: async () => JSON.stringify([cachedPlan()]),
    translate: (_direction, _text, onEvent) => {
      onEvent({ type: 'delta', text: '部分译文' });
      return pending.promise;
    },
  });
  await store.getState().hydrate('conv');
  store.getState().upsertFromSession('conv', [planUpdate()]);
  assert.deepEqual(translations, [{ direction: 'en-zh', text: PLAN }]);
  const translating = store.getState().byConv.conv[0];
  assert.equal(translating.original, PLAN);
  assert.equal(translating.translation, '部分译文');
  assert.equal(translating.working, true);
  assert.notEqual(translating.translatedFrom, PLAN);
  pending.resolve('# 完整计划\n\n保留最终验证步骤');
  await settleTranslation();
  const complete = store.getState().byConv.conv[0];
  assert.equal(complete.translatedFrom, PLAN);
  assert.equal(complete.translation, '# 完整计划\n\n保留最终验证步骤');
  assert.equal(complete.working, false);
  store.getState().upsertFromSession('conv', [planUpdate()]);
  assert.equal(translations.length, 1);
});

test('local plan translation preserves its covered prefix and translates the final unfinished tail', async () => {
  const prefix = '# Plan\n\nFirst completed step.\n\n';
  const tail = 'Final verification step without a trailing period';
  const { store, translations } = await setup({
    mode: 'local',
    load: async () => JSON.stringify([cachedPlan({
      original: prefix, translation: '# 计划\n\n第一步。\n\n', translatedFrom: prefix, done: false,
    })]),
    translate: async () => '最终验证步骤',
  });
  await store.getState().hydrate('conv');
  store.getState().upsertFromSession('conv', [planUpdate(prefix + tail)]);
  await settleTranslation();
  assert.deepEqual(translations, [{ direction: 'en-zh', text: tail }]);
  const message = store.getState().byConv.conv[0];
  assert.equal(message.translation, '# 计划\n\n第一步。\n\n最终验证步骤');
  assert.equal(message.translatedFrom, prefix + tail);
});

function userLine(key, text, overrides = {}) {
  return {
    key, role: 'user', text, ts: null, thinking: null,
    toolCalls: [], toolResult: null, done: true, ...overrides,
  };
}

function assistantLine(key, text, overrides = {}) {
  return {
    key, role: 'assistant', text, ts: null, thinking: null,
    toolCalls: [], toolResult: null, done: false, ...overrides,
  };
}

test('a transcript prompt reconciles with its optimistic bubble and keeps the local key', async () => {
  const { store } = await setup();
  const key = store.getState().addOptimistic('conv', '你好');
  store.getState().markSent('conv', key, 'Hello');
  const line = userLine('uuid-1', '  Hello\n', { ts: '2026-01-01T00:00:00Z' });
  // The JSONL line can land while the outbound translation is still running.
  store.getState().upsertFromSession('conv', [line], false);
  store.getState().upsertFromSession('conv', [line], false);
  const messages = store.getState().byConv.conv;
  assert.equal(messages.length, 1);
  assert.equal(messages[0].key, key, 'in-flight translation callbacks address the local key');
  assert.equal(messages[0].sessionKey, 'uuid-1');
  assert.equal(messages[0].ts, '2026-01-01T00:00:00Z');
  assert.equal(messages[0].original, '你好');
  store.getState().setTranslation('conv', key, 'Hello', '你好');
  assert.equal(store.getState().byConv.conv[0].translation, 'Hello');
});

test('a prompt typed in the terminal becomes its own bubble without image markers', async () => {
  const { store } = await setup();
  const line = userLine('uuid-1', '[Image #1] look   here');
  store.getState().upsertFromSession('conv', [line], false);
  store.getState().upsertFromSession('conv', [line], false);
  const messages = store.getState().byConv.conv;
  assert.equal(messages.length, 1);
  assert.equal(messages[0].key, 'uuid-1');
  assert.equal(messages[0].sessionKey, 'uuid-1');
  assert.equal(messages[0].fromTerminal, true);
  assert.equal(messages[0].original, 'look here');
  assert.equal(messages[0].hadImages, true);

  store.getState().upsertFromSession('conv', [userLine('uuid-2', 'plain text')], false);
  assert.equal(store.getState().byConv.conv[1].hadImages, undefined);
});

test('an image prompt reconciles through its pasted path or attachment marker', async () => {
  for (const text of ['/cache/a.png look', '[Image #1] look']) {
    const { store } = await setup();
    const key = store.getState().addPlain('conv', 'look', ['/cache/a.png']);
    store.getState().upsertFromSession('conv', [userLine('uuid-1', text)], false);
    const messages = store.getState().byConv.conv;
    assert.equal(messages.length, 1, text);
    assert.equal(messages[0].key, key);
    assert.equal(messages[0].sessionKey, 'uuid-1');
    assert.equal(messages[0].original, 'look');
  }
  // Without an image of its own, a bubble never matches by a stripped path.
  const { store } = await setup();
  store.getState().addPlain('conv', 'look');
  store.getState().upsertFromSession('conv', [userLine('uuid-1', '/cache/a.png look')], false);
  assert.equal(store.getState().byConv.conv.length, 2);
});

test('an interrupt marks the unreconciled bubble, not an older one with the same text', async () => {
  const { store } = await setup();
  const first = store.getState().addPlain('conv', 'retry');
  store.getState().upsertFromSession('conv', [userLine('uuid-1', 'retry')], false);
  const second = store.getState().addPlain('conv', 'retry');
  store.getState().upsertFromSession('conv', [userLine('uuid-2', 'retry', { interrupted: true })], false);
  const messages = store.getState().byConv.conv;
  assert.deepEqual(Array.from(messages, (m) => m.key), [first, second]);
  assert.equal(messages[0].sessionKey, 'uuid-1');
  assert.equal(messages[0].interrupted, undefined);
  assert.equal(messages[1].sessionKey, 'uuid-2');
  assert.equal(messages[1].interrupted, true);
});

test('an interrupt update for a known transcript key marks it without adding a bubble', async () => {
  // An older cache holds the turn under its transcript key, with no session key.
  const cached = [cachedPlan({ key: 'uuid-1', role: 'user', original: 'older text', toolCalls: [] })];
  const { store } = await setup({ load: async () => JSON.stringify(cached) });
  await store.getState().hydrate('conv');
  store.getState().upsertFromSession('conv', [userLine('uuid-1', 'different text')], false);
  assert.equal(store.getState().byConv.conv[0].interrupted, undefined);
  store.getState().upsertFromSession(
    'conv', [userLine('uuid-1', 'different text', { interrupted: true })], false,
  );
  const messages = store.getState().byConv.conv;
  assert.equal(messages.length, 1);
  assert.equal(messages[0].original, 'older text');
  assert.equal(messages[0].interrupted, true);
});

test('only a later assistant reply clears an interrupted prompt', async () => {
  const prompt = userLine('uuid-1', 'go', { ts: '2026-01-01T00:00:10Z', interrupted: true });
  for (const [replyAt, interrupted] of [
    ['2026-01-01T00:00:15Z', false],
    ['2026-01-01T00:00:05Z', true],
    [null, true],
  ]) {
    const { store } = await setup();
    store.getState().upsertFromSession('conv', [prompt], false);
    assert.equal(store.getState().byConv.conv[0].interrupted, true);
    store.getState().upsertFromSession('conv', [assistantLine('msg-1', 'ok', { ts: replyAt })], false);
    const messages = store.getState().byConv.conv;
    assert.equal(messages.length, 2);
    assert.equal(messages[0].interrupted, interrupted, `reply at ${replyAt}`);
    assert.equal(messages[1].original, 'ok');
  }
});

test('block-scoped assistant lines merge without wiping text, thinking or completion', async () => {
  const { store } = await setup();
  const upsert = (overrides) =>
    store.getState().upsertFromSession('conv', [assistantLine('msg-1', '', overrides)], false);
  upsert({ thinking: 'Consider the request.' });
  upsert({ text: 'Final answer.', done: true });
  // A late block for the same message id carries neither text nor thinking.
  upsert({ toolCalls: [{ id: 'read', name: 'Read', label: 'file', detail: 'file' }] });
  const messages = store.getState().byConv.conv;
  assert.equal(messages.length, 1);
  assert.equal(messages[0].original, 'Final answer.');
  assert.equal(messages[0].thinking, 'Consider the request.');
  assert.equal(messages[0].done, true);
  assert.deepEqual(Array.from(messages[0].toolCalls, (call) => call.id), ['read']);
});
