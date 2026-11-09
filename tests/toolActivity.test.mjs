import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, SourceTextModule, SyntheticModule } from 'node:vm';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as react from 'react';
import * as jsxRuntime from 'react/jsx-runtime';
import { transformWithOxc } from 'vite';

// Render the actual activity component without a desktop bridge or new test dependencies.
const context = createContext({ URL });
const files = ['src/toolActivity.ts', 'src/format.ts', 'src/links.ts', 'src/components/ContentLink.tsx', 'src/components/Icons.tsx', 'src/components/ToolActivity.tsx'];
const modules = new Map(await Promise.all(files.map(async (file) => {
  const url = new URL(`../${file}`, import.meta.url);
  const source = await readFile(url, 'utf8');
  const { code } = await transformWithOxc(source, url.pathname, { jsx: { runtime: 'automatic' } });
  return [url.href, new SourceTextModule(code, { context, identifier: url.href })];
})));
const externals = new Map(Object.entries({
  react, 'react/jsx-runtime': jsxRuntime,
  '@tauri-apps/plugin-opener': { openUrl: async () => {}, openPath: async () => {} },
  '@tauri-apps/api/path': { homeDir: async () => '/Users/test', resolve: async (...parts) => parts.join('/') },
}).map(([name, exports]) => [
  name, new SyntheticModule(Object.keys(exports), function () {
    for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
  }, { context }),
]));
const component = modules.get(new URL('../src/components/ToolActivity.tsx', import.meta.url).href);
await component.link((specifier, importer) => {
  if (externals.has(specifier)) return externals.get(specifier);
  const path = new URL(specifier, importer.identifier).href;
  const module = modules.get(`${path}.ts`) ?? modules.get(`${path}.tsx`);
  assert.ok(module, `Unexpected import: ${specifier}`);
  return module;
});
await component.evaluate();
const ToolActivity = component.namespace.default;
const { groupToolActivity, summarizeToolActivity } = modules.get(new URL('../src/toolActivity.ts', import.meta.url).href).namespace;

const tool = (id, name = 'Read', overrides = {}) => ({ id, name, detail: `${id}.ts`, result: 'output', ...overrides });
const message = (key, toolCalls = [], overrides = {}) => ({
  key, role: 'assistant', original: '', translation: null, working: false,
  error: null, ts: null, thinking: null, toolCalls, done: true, ...overrides,
});

test('consecutive transcript messages share one group without losing call order or results', () => {
  const messages = [
    message('read', [tool('file')]),
    message('db', [tool('describe', 'mcp__dbx__describe_table'), tool('query', 'mcp__dbx__execute_query')]),
  ];
  const rows = groupToolActivity(messages);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].type, 'tools');
  assert.equal(rows[0].messages[0], messages[0]);
  assert.equal(rows[0].messages[1], messages[1]);
  assert.equal(summarizeToolActivity(rows[0].messages), '读取了 1 个文件，使用了 2 个工具');
  assert.equal(messages.length, 2, 'display grouping must not mutate transcript history');
});

test('user prompts, replies and plan approval calls separate activity groups', async (t) => {
  const boundaries = [
    message('user', [], { role: 'user', original: 'Continue' }),
    message('reply', [], { original: 'Here are the results.' }),
    message('translated', [], { translation: '结果如下。' }),
    message('plan', [tool('exit', 'ExitPlanMode')], { original: 'The complete plan' }),
    message('pending-plan', [tool('exit', 'ExitPlanMode', { result: undefined })]),
    message('mixed', [tool('mixed-read')], { original: 'Text accompanying a tool' }),
  ];
  for (const boundary of boundaries) {
    await t.test(boundary.key, () => {
      const rows = groupToolActivity([message('before', [tool('a')]), boundary, message('after', [tool('b')])]);
      assert.equal(rows.length, 3);
      assert.equal(rows[0].type, 'tools');
      assert.equal(rows[1].type, 'message');
      assert.equal(rows[1].message, boundary);
      assert.equal(rows[2].type, 'tools');
    });
  }
});

test('thinking between tools stays in the same collapsed group and contributes to its summary', () => {
  const thinking = message('thinking', [], { thinking: 'Check the schema before querying.' });
  const messages = [message('a', [tool('a')]), thinking, message('b', [tool('b')])];
  const rows = groupToolActivity(messages);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].messages[1], thinking);
  assert.equal(summarizeToolActivity(rows[0].messages), '思考了 1 次，读取了 2 个文件');
  const alone = groupToolActivity([thinking]);
  assert.equal(alone[0].type, 'tools');
  assert.equal(alone[0].messages[0], thinking);
  assert.equal(summarizeToolActivity(alone[0].messages), '思考了 1 次');
});

test('thinking snapshots update one count and appending more thinking retains group identity', () => {
  const initial = groupToolActivity([message('thinking', [], { thinking: 'Inspect' })]);
  const updated = groupToolActivity([
    message('thinking', [], { thinking: 'Inspect the full schema.', done: false }),
    message('read', [tool('file')], { thinking: 'Now check the source.' }),
    message('blank', [], { thinking: '  ' }),
  ]);
  assert.equal(initial[0].key, updated[0].key);
  assert.equal(summarizeToolActivity(updated[0].messages), '思考了 2 次，读取了 1 个文件');
});

test('attachments, message errors, and translation progress remain visible after their activity', () => {
  for (const overrides of [
    { images: ['image.png'] }, { files: ['file.txt'] }, { hadImages: true },
    { error: 'Translation failed' }, { working: true },
  ]) {
    const current = message('current', [tool('a')], overrides);
    const rows = groupToolActivity([current]);
    assert.equal(rows.length, 2);
    assert.equal(rows[0].type, 'tools');
    assert.equal(rows[1].type, 'message');
    assert.equal(rows[1].message, current);
    assert.equal(rows[1].hideActivity, true, 'the visible body must not duplicate its folded activity');
  }
});

test('thinking attached to a reply joins preceding tools without hiding or duplicating the reply', () => {
  const reply = message('reply', [tool('b')], { original: 'Here are the findings.', thinking: 'Review the file icon assets.' });
  const messages = [message('a', [tool('a')]), reply, message('c', [tool('c')])];
  const rows = groupToolActivity(messages);
  assert.equal(rows.length, 3);
  assert.equal(rows[0].messages[1], reply);
  assert.equal(summarizeToolActivity(rows[0].messages), '思考了 1 次，读取了 2 个文件');
  assert.equal(rows[1].message, reply);
  assert.equal(rows[1].hideActivity, true);
  assert.equal(rows[2].messages[0], messages[2], 'a visible reply still separates later activity');
  assert.equal(reply.thinking, 'Review the file icon assets.', 'the original record remains intact');
});

test('appended calls and result updates retain the group identity', () => {
  const pending = message('a', [tool('a', 'Read', { result: undefined })]);
  const initial = groupToolActivity([pending]);
  const complete = groupToolActivity([{ ...pending, toolCalls: [tool('a')] }, message('b', [tool('b', 'Bash')])]);
  assert.equal(initial[0].key, complete[0].key);
  assert.equal(initial[0].messages[0].toolCalls[0].result, undefined);
  assert.equal(complete[0].messages[0].toolCalls[0].result, 'output');
});

test('file totals count distinct paths and unfinished or failed reads do not claim completion', () => {
  const summary = (calls) => summarizeToolActivity([message('tools', calls)]);
  assert.equal(summary([tool('a', 'Read', { detail: '/src/file.ts' }), tool('b', 'Read', { detail: '/src/file.ts' })]), '读取了 1 个文件');
  assert.equal(summary([tool('a', 'Read', { result: undefined })]), '读取 1 个文件');
  assert.equal(summary([tool('a', 'Read', { isError: true })]), '读取 1 个文件');
  assert.equal(summary([tool('a', 'Bash', { result: '' })]), '使用了 1 个工具');
  assert.equal(summary([tool('a', 'Bash', { result: undefined })]), '使用 1 个工具');
});

test('the rendered disclosure starts collapsed and keeps commands, outputs and thinking accessible', () => {
  const markup = renderToStaticMarkup(createElement(ToolActivity, {
    messages: [
      message('a', [tool('file', 'Read', { detail: '/src/file.ts', result: 'File contents' })], { thinking: 'Inspect the file.' }),
      message('b', [tool('shell', 'Bash', { detail: 'git status', result: '' })]),
    ],
  }));
  assert.match(markup, /^<details class="tool-activity">/);
  assert.match(markup, /思考了 1 次，读取了 1 个文件，使用了 1 个工具/);
  assert.match(markup, /class="tool-activity-list"/);
  assert.match(markup, /File contents/);
  assert.match(markup, /git status/);
  assert.match(markup, /（无输出）/);
  assert.ok(markup.indexOf('Inspect the file.') < markup.indexOf('/src/file.ts'));
  assert.doesNotMatch(markup, /项等待输出|项失败/);
});

test('thinking-only activity has a counted summary and keeps its full text inside the disclosure', () => {
  const markup = renderToStaticMarkup(createElement(ToolActivity, {
    messages: [
      message('a', [], { thinking: 'Waiting for agent results.' }),
      message('b', [], { thinking: 'Reviewing file icon assets.' }),
      message('blank', [], { thinking: '  ' }),
    ],
  }));
  const summaryEnd = markup.indexOf('</summary>');
  assert.match(markup.slice(0, summaryEnd), /思考了 2 次/);
  assert.doesNotMatch(markup.slice(0, summaryEnd), /Waiting for agent results|Reviewing file icon assets/);
  assert.equal((markup.match(/class="thinking-line"/g) ?? []).length, 2);
  assert.ok(markup.indexOf('class="thinking-line"') > markup.indexOf('class="tool-activity-list"'));
});

test('failure and pending counts stay visible in the collapsed summary', () => {
  const markup = renderToStaticMarkup(createElement(ToolActivity, {
    messages: [message('a', [tool('failed', 'Bash', { isError: true, result: 'Permission denied' }), tool('pending', 'Read', { result: undefined })])],
  }));
  const summary = markup.slice(0, markup.indexOf('</summary>'));
  assert.match(summary, /1 项失败/);
  assert.match(summary, /1 项等待输出/);
  assert.match(markup, /Permission denied/);
  assert.match(markup, /等待输出…/);
});

test('plan step labels and legacy empty tool inputs keep their existing presentation', () => {
  const markup = renderToStaticMarkup(createElement(ToolActivity, {
    messages: [message('a', [
      tool('write', 'Write', { detail: '/Users/test/.claude/plans/example.md' }),
      tool('enter', 'EnterPlanMode', { detail: '{}' }),
    ])],
  }));
  assert.match(markup, /Update Plan/);
  assert.match(markup, /Enter Plan Mode/);
  assert.doesNotMatch(markup, /\{\}/);
});
