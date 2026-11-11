import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, SourceTextModule, SyntheticModule } from 'node:vm';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import * as react from 'react';
import * as jsxRuntime from 'react/jsx-runtime';
import { transformWithOxc } from 'vite';

const context = createContext({ URL });
const files = ['src/gitCommitDetails.ts', 'src/links.ts', 'src/components/GitCommitCard.tsx', 'src/components/ContentLink.tsx', 'src/components/Icons.tsx'];
const modules = new Map(await Promise.all(files.map(async (file) => {
  const url = new URL(`../${file}`, import.meta.url);
  const { code } = await transformWithOxc(await readFile(url, 'utf8'), url.pathname, { jsx: { runtime: 'automatic' } });
  return [url.href, new SourceTextModule(code, { context, identifier: url.href })];
})));
const externalExports = {
  react, 'react/jsx-runtime': jsxRuntime,
  '@tauri-apps/plugin-opener': { openUrl: async () => {}, openPath: async () => {} },
  '@tauri-apps/api/path': { homeDir: async () => '/Users/test', resolve: async (...parts) => parts.join('/') },
};
const externals = new Map(Object.entries(externalExports).map(([name, exports]) => [name,
  new SyntheticModule(Object.keys(exports), function () {
    for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
  }, { context }),
]));
const getModule = (file) => modules.get(new URL(`../src/${file}`, import.meta.url).href);
const root = getModule('components/GitCommitCard.tsx');
await root.link((specifier, importer) => {
  if (externals.has(specifier)) return externals.get(specifier);
  const path = new URL(specifier, importer.identifier).href;
  const module = modules.get(`${path}.ts`) ?? modules.get(`${path}.tsx`);
  assert.ok(module, `Unexpected import: ${specifier}`);
  return module;
});
await root.evaluate();
const { createCommitDetailsCache, formatCommitDate, formatCommitAge } = getModule('gitCommitDetails.ts').namespace;
const { GitCommitCardContent } = root.namespace;
const commit = {
  oid: 'a'.repeat(40), parents: ['b'.repeat(40)], subject: 'chore(release): 1.4.0',
  authorName: 'Yukuiii', authorTime: new Date(2026, 10, 10, 4, 31).getTime() / 1000,
};
const details = {
  oid: commit.oid, message: 'chore(release): 1.4.0\n\n发布说明与完整提交正文。',
  filesChanged: 5, insertions: 6, deletions: 6, githubUrl: `https://github.com/Yukuiii/YiTerm/commit/${commit.oid}`,
};
const refs = [
  { name: 'refs/remotes/origin/main', shortName: 'origin/main', kind: 'remote', oid: commit.oid },
  { name: 'refs/heads/chore/release-1.4.0', shortName: 'chore/release-1.4.0', kind: 'branch', oid: commit.oid },
  { name: 'refs/tags/v1.4.0', shortName: 'v1.4.0', kind: 'tag', oid: commit.oid },
];
const render = (overrides = {}) => renderToStaticMarkup(createElement(GitCommitCardContent, {
  commit, details, refs, isHead: false, error: null, copyState: 'idle', onCopy() {}, onRetry() {}, ...overrides,
}));

test('commit hover card renders complete message, change totals, refs and GitHub destination', () => {
  const html = render();
  for (const text of ['Yukuiii', 'chore(release): 1.4.0', '发布说明与完整提交正文。', '5 个文件改动', '6 行新增(+)', '6 行删除(-)', 'origin/main', 'chore/release-1.4.0', 'v1.4.0']) {
    assert.ok(html.includes(text), text);
  }
  assert.match(html, /aria-label="复制完整提交 SHA"/);
  assert.match(html, /<code>aaaaaaa<\/code>/);
  assert.match(html, new RegExp(`href="${details.githubUrl}"`));
  assert.match(html, /rel="noopener noreferrer"/);
});

test('hover card distinguishes loading and failure from an empty commit', () => {
  assert.match(render({ details: null }), /正在读取改动统计/);
  const failed = render({ details: null, error: 'Git failed' });
  assert.match(failed, /role="alert"/);
  assert.match(failed, /重试/);
  assert.doesNotMatch(failed, /0 个文件改动/);
  const empty = render({ details: { ...details, filesChanged: 0, insertions: 0, deletions: 0, githubUrl: null } });
  assert.match(empty, /0 个文件改动/);
  assert.doesNotMatch(empty, /在 GitHub 中打开|<a /);
});

test('commit messages and ref names remain escaped text', () => {
  const html = render({
    details: { ...details, message: '<script>alert(1)</script>\n\n**literal text**' },
    refs: [{ ...refs[0], shortName: '<img src=x onerror=alert(1)>' }], isHead: true,
  });
  assert.match(html, /&lt;script&gt;alert\(1\)&lt;\/script&gt;/);
  assert.match(html, /&lt;img/);
  assert.doesNotMatch(html, /<script>|<img /);
  assert.match(html, /HEAD/);
  assert.match(html, /\*\*literal text\*\*/);
});

test('merge statistics explain first-parent comparison and copying reports its outcome', () => {
  assert.match(render({ commit: { ...commit, parents: ['b'.repeat(40), 'c'.repeat(40)] } }), /与第一父提交相比/);
  assert.match(render({ copyState: 'copied' }), /role="status">已复制/);
  assert.match(render({ copyState: 'failed' }), /role="alert">复制失败，请重试/);
});

test('commit dates include an absolute date and a deterministic relative age', () => {
  const time = commit.authorTime;
  assert.match(formatCommitDate(time), /2026年11月10日/);
  assert.equal(formatCommitDate(NaN), '未知时间');
  assert.equal(formatCommitAge(time, time * 1000), '刚刚');
  assert.equal(formatCommitAge(time, (time + 86400) * 1000), '1天前');
  assert.equal(formatCommitAge(time, (time - 7200) * 1000), '2小时后');
  assert.equal(formatCommitAge(NaN), '未知时间');
});

test('commit detail reads deduplicate in flight and reuse successful results', async () => {
  let resolve;
  let calls = 0;
  const load = createCommitDetailsCache(() => { calls++; return new Promise((done) => { resolve = done; }); });
  const first = load(commit.oid);
  assert.equal(load(commit.oid), first);
  assert.equal(calls, 1);
  resolve(details);
  assert.equal(await first, details);
  assert.equal(await load(commit.oid), details);
  assert.equal(calls, 1);
});

test('failed commit reads are evicted so an explicit retry can recover', async () => {
  let calls = 0;
  const load = createCommitDetailsCache(async () => {
    if (++calls === 1) throw new Error('Git failed');
    return details;
  });
  await assert.rejects(load(commit.oid), /Git failed/);
  assert.equal(await load(commit.oid), details);
  assert.equal(calls, 2);
});

test('the detail cache bounds memory across a long history browsing session', async () => {
  let calls = 0;
  const load = createCommitDetailsCache(async (oid) => { calls++; return { ...details, oid }; });
  for (let i = 0; i < 101; i++) await load(String(i));
  await load('100');
  assert.equal(calls, 101);
  await load('0');
  assert.equal(calls, 102);
});
