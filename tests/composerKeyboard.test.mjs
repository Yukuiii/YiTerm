import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule, SyntheticModule } from 'node:vm';

const source = stripTypeScriptTypes(await readFile(new URL('../src/hooks/useComposerKeyboard.ts', import.meta.url), 'utf8'));

async function setup() {
  const context = createContext({});
  const composingRef = { current: false };
  let submissions = 0;
  const module = new SourceTextModule(source, { context });
  await module.link((specifier) => {
    assert.equal(specifier, 'react');
    return new SyntheticModule(['useRef'], function () {
      this.setExport('useRef', () => composingRef);
    }, { context });
  });
  await module.evaluate();
  const render = () => module.namespace.useComposerKeyboard(() => { submissions++; });
  return { handlers: render(), render, submissions: () => submissions };
}

function keyEvent(overrides = {}, native = {}) {
  return {
    key: 'Enter', shiftKey: false, defaultPrevented: false,
    nativeEvent: { isComposing: false, keyCode: 13, ...native },
    preventDefault() { this.defaultPrevented = true; },
    ...overrides,
  };
}

test('ordinary Enter submits and prevents the textarea newline', async () => {
  const env = await setup();
  const event = keyEvent();
  env.handlers.onKeyDown(event);
  assert.equal(env.submissions(), 1);
  assert.equal(event.defaultPrevented, true);
});

test('Shift+Enter and ordinary typing retain native editing behavior', async () => {
  const env = await setup();
  for (const event of [keyEvent({ shiftKey: true }), keyEvent({ key: 'a' }), keyEvent({ key: 'Escape' })]) {
    env.handlers.onKeyDown(event);
    assert.equal(event.defaultPrevented, false);
  }
  assert.equal(env.submissions(), 0);
});

test('native composing Enter confirms candidates without submitting', async () => {
  const env = await setup();
  const event = keyEvent({}, { isComposing: true });
  env.handlers.onKeyDown(event);
  assert.equal(env.submissions(), 0);
  assert.equal(event.defaultPrevented, false, 'the IME must receive the confirming Enter');
});

test('composition events guard Enter even when the native composing flag is false', async () => {
  const env = await setup();
  env.handlers.onCompositionStart();
  const event = keyEvent();
  env.handlers.onKeyDown(event);
  assert.equal(env.submissions(), 0);
  assert.equal(event.defaultPrevented, false);
  env.handlers.onCompositionEnd();
  env.handlers.onKeyDown(keyEvent());
  assert.equal(env.submissions(), 1, 'a separate Enter after composition submits immediately');
});

test('WebKit compositionend before the confirming keydown does not send the prompt', async () => {
  const env = await setup();
  env.handlers.onCompositionStart();
  env.handlers.onCompositionEnd();
  const confirming = keyEvent({}, { keyCode: 229, isComposing: false });
  env.handlers.onKeyDown(confirming);
  assert.equal(env.submissions(), 0);
  assert.equal(confirming.defaultPrevented, false);
  env.handlers.onKeyDown(keyEvent());
  assert.equal(env.submissions(), 1);
});

test('the IME keycode protects confirmation even when compositionstart was not observed', async () => {
  const env = await setup();
  const event = keyEvent({}, { keyCode: 229 });
  env.handlers.onKeyDown(event);
  assert.equal(env.submissions(), 0);
  assert.equal(event.defaultPrevented, false);
});

test('composition tracking survives the rerenders caused by editing the draft', async () => {
  const env = await setup();
  env.handlers.onCompositionStart();
  const next = env.render();
  next.onKeyDown(keyEvent());
  assert.equal(env.submissions(), 0);
  next.onCompositionEnd();
  next.onKeyDown(keyEvent());
  assert.equal(env.submissions(), 1);
});

test('an Enter already handled by another listener is not submitted again', async () => {
  const env = await setup();
  env.handlers.onKeyDown(keyEvent({ defaultPrevented: true }));
  assert.equal(env.submissions(), 0);
});
