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
async function setup({ save = async () => {} } = {}) {
  const timers = new Map();
  const writes = [];
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
      chatLoad: async () => null,
      chatSave: (convId, json) => {
        writes.push({ convId, json });
        return save(convId, json);
      },
      debugLog: async () => {},
      hasCJK: () => false,
      translate: async () => { throw new Error('Unexpected translation'); },
    },
    './settings': {
      useSettingsStore: { getState: () => ({ settings: null }) },
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
