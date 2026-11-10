import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, SourceTextModule, SyntheticModule } from 'node:vm';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as react from 'react';
import * as jsxRuntime from 'react/jsx-runtime';
import * as reactMarkdown from 'react-markdown';
import * as remarkGfm from 'remark-gfm';
import { transformWithOxc } from 'vite';

const context = createContext({ URL });
const files = [
  'src/links.ts', 'src/format.ts', 'src/toolActivity.ts',
  'src/components/ContentLink.tsx', 'src/components/Markdown.tsx',
  'src/components/Icons.tsx', 'src/components/ToolActivity.tsx',
  'src/components/MessageBubble.tsx', 'src/components/PlanCard.tsx',
];
const modules = new Map(await Promise.all(files.map(async (file) => {
  const url = new URL(`../${file}`, import.meta.url);
  const { code } = await transformWithOxc(await readFile(url, 'utf8'), url.pathname, { jsx: { runtime: 'automatic' } });
  return [url.href, new SourceTextModule(code, { context, identifier: url.href })];
})));
const exportsByModule = {
  react, 'react/jsx-runtime': jsxRuntime, 'react-markdown': reactMarkdown, 'remark-gfm': remarkGfm,
  '@tauri-apps/plugin-opener': { openUrl: async () => {}, openPath: async () => {} },
  '@tauri-apps/api/path': { homeDir: async () => '/Users/test', resolve: async (...parts) => parts.join('/') },
  '@tauri-apps/api/core': { convertFileSrc: (path) => path },
};
const externals = new Map(Object.entries(exportsByModule).map(([name, exports]) => [
  name, new SyntheticModule(Object.keys(exports), function () {
    for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
  }, { context }),
]));
const getModule = (file) => modules.get(new URL(`../src/${file}`, import.meta.url).href);
const root = getModule('components/MessageBubble.tsx');
await root.link((specifier, importer) => {
  if (externals.has(specifier)) return externals.get(specifier);
  const path = new URL(specifier, importer.identifier).href;
  const module = modules.get(`${path}.ts`) ?? modules.get(`${path}.tsx`);
  assert.ok(module, `Unexpected import: ${specifier}`);
  return module;
});
await root.evaluate();
const MessageBubble = root.namespace.default;
const Markdown = getModule('components/Markdown.tsx').namespace.default;
const ToolActivity = getModule('components/ToolActivity.tsx').namespace.default;
const { LinkedText, ContentLinkProvider } = getModule('components/ContentLink.tsx').namespace;
const { externalUrl, fileReference, splitLinks } = getModule('links.ts').namespace;
const render = (Component, props) => renderToStaticMarkup(createElement(Component, props));
const urls = (text) => [...splitLinks(text)].flatMap((part) => part.href ? [part.href] : []);
const message = (overrides = {}) => ({
  key: 'message', role: 'assistant', original: '', translation: null, working: false,
  error: null, ts: null, thinking: null, toolCalls: [], done: true, ...overrides,
});

test('normalizes supported external destinations and leaves unsupported schemes inactive', () => {
  assert.equal(externalUrl('WWW.Example.com/docs'), 'https://www.example.com/docs');
  assert.equal(externalUrl('//example.com/docs'), 'https://example.com/docs');
  assert.equal(externalUrl('https://例子.测试/文档'), new URL('https://例子.测试/文档').href);
  assert.equal(externalUrl('mailto:dev@example.com'), 'mailto:dev@example.com');
  assert.equal(externalUrl('tel:+123456789'), 'tel:+123456789');
  for (const value of [undefined, '', 'https://', 'javascript:alert(1)', 'data:text/html,test', 'file:///tmp/file', '/relative/path']) {
    assert.equal(externalUrl(value), null);
  }
});

test('literal URL detection preserves all text, indentation, punctuation and line breaks', () => {
  const text = '  **literal**\n查看：https://example.com/docs。\n  www.example.org/path,\n';
  assert.equal([...splitLinks(text)].map((part) => part.text).join(''), text);
  assert.deepEqual(urls(text), ['https://example.com/docs', 'https://www.example.org/path']);
});

test('balanced URL parentheses and IPv6 are preserved while prose delimiters are excluded', () => {
  const text = '(https://example.com/wiki/Function_(math)). [http://[::1]:3000/docs]';
  assert.deepEqual(urls(text), ['https://example.com/wiki/Function_(math)', 'http://[::1]:3000/docs']);
  assert.equal([...splitLinks(text)].map((part) => part.text).join(''), text);
  assert.deepEqual(urls('https:// followed by regular words'), []);
});

test('plain-text messages gain external anchors without interpreting their Markdown or HTML', () => {
  const html = render(LinkedText, { children: '**keep this** <b>https://example.com/docs</b>' });
  assert.match(html, /\*\*keep this\*\*/);
  assert.match(html, /&lt;b&gt;/);
  assert.match(html, /href="https:\/\/example.com\/docs"/);
  assert.match(html, /target="_blank"/);
  assert.match(html, /rel="noopener noreferrer"/);
  assert.doesNotMatch(html, /<b>|<strong>/);
});

test('Markdown links retain explicit destinations and code labels never create nested anchors', () => {
  const html = render(Markdown, { children: '[`https://label.example/`](https://target.example/path?x=1&y=2) and [exact](https://target.example/query!)' });
  assert.equal((html.match(/<a /g) ?? []).length, 2);
  assert.match(html, /href="https:\/\/target.example\/path\?x=1&amp;y=2"/);
  assert.match(html, /href="https:\/\/target.example\/query!"/);
  assert.doesNotMatch(html, /href="https:\/\/label.example/);
});

test('bare Markdown URLs exclude adjacent Chinese sentence punctuation', () => {
  const html = render(Markdown, { children: '请打开：https://example.com/path。' });
  assert.match(html, /href="https:\/\/example.com\/path"/);
  assert.doesNotMatch(html, /path%E3%80%82/);
  assert.match(html, /<\/a>。/);
});

test('inline and fenced code URLs become clickable while their code markup stays intact', () => {
  const html = render(Markdown, { children: '`https://inline.example/docs`\n\n```sh\ncurl "https://code.example/api"\n```' });
  assert.match(html, /<code><a [^>]*href="https:\/\/inline.example\/docs"/);
  assert.match(html, /<pre><code class="language-sh">/);
  assert.match(html, /href="https:\/\/code.example\/api"/);
  assert.match(html, /curl/);
});

test('user prompts and their English translation both expose clickable URLs', () => {
  const html = render(MessageBubble, {
    msg: message({ role: 'user', original: '打开 https://user.example/docs', translation: 'Open https://translation.example/docs' }),
    onRetry() {}, onSendAsIs() {},
  });
  assert.match(html, /href="https:\/\/user.example\/docs"/);
  assert.match(html, /href="https:\/\/translation.example\/docs"/);
});

test('expanded thinking, tool commands and outputs all expose clickable URLs', () => {
  const html = render(ToolActivity, { messages: [message({
    thinking: 'Read https://thinking.example/docs first.',
    toolCalls: [{ id: 'tool', name: 'Bash', detail: 'curl https://command.example/api', result: 'See https://output.example/results' }],
  })] });
  for (const host of ['thinking', 'command', 'output']) assert.ok(html.includes(`href="https://${host}.example/`));
});

test('plan content shares working Markdown links and unsupported destinations do not navigate', () => {
  const html = render(MessageBubble, {
    msg: message({ original: '## Plan\n\nSee [the documentation](https://plan.example/docs).', toolCalls: [{ id: 'plan', name: 'ExitPlanMode', detail: '' }] }),
    onRetry() {}, onSendAsIs() {},
  });
  assert.match(html, /class="plan-card"/);
  assert.match(html, /href="https:\/\/plan.example\/docs"/);
  const unsafe = render(Markdown, { children: '[unsafe](javascript:alert%281%29) [custom](custom:some/file.ts)' });
  assert.doesNotMatch(unsafe, /<a /);
});

test('fragment links remain inside the document rather than going through the desktop opener', () => {
  const html = render(Markdown, { children: '[Jump](#section)' });
  assert.match(html, /href="#section"/);
  assert.doesNotMatch(html, /target="_blank"/);
});

const renderInProject = (Component, props) => renderToStaticMarkup(createElement(ContentLinkProvider,
  { folder: '/workspace/project' }, createElement(Component, props)));

test('local references separate source line and column suffixes from the actual filename', () => {
  for (const [href, path, line, column] of [
    ['/Users/test/src/TerminalPanel.tsx:152', '/Users/test/src/TerminalPanel.tsx', 152, undefined],
    ['src/TerminalPanel.tsx:152:7', 'src/TerminalPanel.tsx', 152, 7],
    ['TerminalPanel.tsx:152', 'TerminalPanel.tsx', 152, undefined],
    ['src/TerminalPanel.tsx#L152', 'src/TerminalPanel.tsx', 152, undefined],
    ['file:///Users/test/My%20Project/file.ts#L42C3', '/Users/test/My Project/file.ts', 42, 3],
    ['~/projects/.env', '~/projects/.env', undefined, undefined],
  ]) {
    const ref = fileReference(href);
    assert.ok(ref, href);
    assert.equal(ref.path, path);
    assert.equal(ref.line, line);
    assert.equal(ref.column, column);
  }
  for (const href of ['Observer', 'ResizeObserver', 'https://example.com/src/App.tsx', 'javascript:some/file.ts', 'file://remote-server/file.ts', '/tmp/file.ts:0']) {
    assert.equal(fileReference(href), null, href);
  }
});

test('bare file-like tokens are never turned into links, only web URLs are', () => {
  for (const text of [
    'Built with Node.js, Vue.js, Next.js, Express.js and Three.js.',
    'Read src/App.tsx:12 and package.json next.',
    'Read `src/App.tsx:12` and `package.json` next.',
  ]) {
    assert.deepEqual(urls(text), []);
    assert.equal([...splitLinks(text)].map((part) => part.text).join(''), text);
  }
});

test('the reported Electron reply keeps Node.js as text instead of a failing file tag', () => {
  const text = '- **Shell:** Electron (Chromium + Node.js; ~v42 as of the mid-2026 build), chosen explicitly so the same UI could ship on Windows quickly';
  const html = renderInProject(Markdown, { children: text });
  assert.doesNotMatch(html, /<a /);
  assert.doesNotMatch(html, /打开失败/);
  assert.match(html, /Node\.js/);
  const inline = renderInProject(Markdown, { children: 'The runtime is `Node.js`.' });
  assert.doesNotMatch(inline, /<a /);
  assert.match(inline, /<code>Node\.js<\/code>/);
});

test('the exact reported English and Chinese replies retain a clickable local Markdown target', () => {
  const target = '/Users/admin/code/yiterm/src/components/TerminalPanel.tsx:152';
  for (const text of [
    `There’s no file named \`Observer\` in this project. The observer implementation is in [src/components/TerminalPanel.tsx:152](${target}), where a \`ResizeObserver\` handles terminal resizing.`,
    `该项目中没有名为 \`Observer\` 的文件。观察者实现位于 [src/components/TerminalPanel.tsx:152](${target})，其中 \`ResizeObserver\` 负责处理终端窗口的大小调整。`,
  ]) {
    const html = renderInProject(Markdown, { children: text });
    assert.equal((html.match(/<a /g) ?? []).length, 1);
    assert.match(html, /data-file-path="\/Users\/admin\/code\/yiterm\/src\/components\/TerminalPanel.tsx"/);
    assert.match(html, /data-file-line="152"/);
    assert.match(html, />src\/components\/TerminalPanel.tsx:152<\/a>/);
    assert.match(html, /<code>Observer<\/code>/);
    assert.doesNotMatch(html, /<a [^>]*>ResizeObserver/);
  }
});

test('bare paths and inline code file references stay plain text without an explicit Markdown link', () => {
  const text = '观察者位于 src/components/TerminalPanel.tsx:152，配置见 `package.json`。';
  const html = renderInProject(Markdown, { children: text });
  assert.doesNotMatch(html, /<a /);
  assert.match(html, /<code>package\.json<\/code>/);
  assert.match(html, /src\/components\/TerminalPanel\.tsx:152/);
  const urlsOnly = 'Read src/App.tsx:12:3, then https://example.com/src/App.tsx:12.';
  assert.equal([...splitLinks(urlsOnly)].map((part) => part.text).join(''), urlsOnly);
  assert.deepEqual(urls(urlsOnly), ['https://example.com/src/App.tsx:12']);
});

test('file URL targets decode spaces and preserve explicit labels without nested anchors', () => {
  const html = renderInProject(Markdown, { children: '[`source file`](file:///Users/test/My%20Project/file.ts#L12)' });
  assert.equal((html.match(/<a /g) ?? []).length, 1);
  assert.match(html, /data-file-path="\/Users\/test\/My Project\/file.ts"/);
  assert.match(html, /data-file-line="12"/);
  assert.match(html, /<code>source file<\/code>/);
});

test('relative references need a project folder rather than guessing from the webview URL', () => {
  const html = render(Markdown, { children: '[source](src/App.tsx:12)' });
  assert.doesNotMatch(html, /<a /);
  const contextual = renderInProject(Markdown, { children: '[source](src/App.tsx:12)' });
  assert.match(contextual, /data-file-path="src\/App.tsx"/);
});

test('explicit Markdown file links work in lists and tables while bare paths stay text', () => {
  const html = renderInProject(Markdown, { children: [
    '- src/App.tsx:12', '- [App source](src/App.tsx:12)', '',
    '| File |', '| --- |', '| src/main.tsx |', '| [Main](src/main.tsx) |', '',
    '[src/label.ts](https://example.com/source) [named](../shared/config.json:8)',
  ].join('\n') });
  assert.equal((html.match(/data-file-path="src\/App\.tsx"/g) ?? []).length, 1);
  assert.equal((html.match(/data-file-path="src\/main\.tsx"/g) ?? []).length, 1);
  assert.match(html, /<li>src\/App\.tsx:12<\/li>/);
  assert.match(html, /<td>src\/main\.tsx<\/td>/);
  assert.match(html, /href="https:\/\/example.com\/source"/);
  assert.match(html, /data-file-path="..\/shared\/config.json"/);
  assert.doesNotMatch(html, /data-file-path="src\/label.ts"/);
});
