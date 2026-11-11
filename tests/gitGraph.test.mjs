import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule } from 'node:vm';

const graphModule = new SourceTextModule(stripTypeScriptTypes(
  await readFile(new URL('../src/gitGraph.ts', import.meta.url), 'utf8'),
));
await graphModule.link(() => { throw new Error('Unexpected runtime import'); });
await graphModule.evaluate();
const { layoutGitGraph, graphPath } = graphModule.namespace;
const plain = (value) => JSON.parse(JSON.stringify(value));
const commit = (oid, ...parents) => ({ oid, parents, subject: oid, authorName: 'Author', authorTime: 1 });

function verifyConnections(commits) {
  const { rows } = plain(layoutGitGraph(commits));
  const boundaries = (segments, start) => [...new Set(segments
    .filter((edge) => start ? edge.start === 'top' : edge.end === 'bottom')
    .map((edge) => `${start ? edge.from : edge.to}:${edge.color}`))].sort();
  assert.equal(rows.length, commits.length);
  for (let index = 0; index < rows.length; index++) {
    const row = rows[index];
    const parents = [...new Set(commits[index].parents)];
    const outgoing = row.segments.filter((edge) => edge.start === 'node');
    assert.equal(outgoing.length, parents.length, `Parents of ${row.oid}`);
    if (index) assert.deepEqual(boundaries(row.segments, true), boundaries(rows[index - 1].segments, false));
    for (const [parentIndex, parent] of parents.entries()) {
      const target = outgoing[parentIndex].to;
      for (let next = index + 1; next < rows.length; next++) {
        const continuation = rows[next].segments.find((edge) => edge.start === 'top' && edge.from === target);
        assert.ok(continuation, `${row.oid} -> ${parent} lost at ${rows[next].oid}`);
        assert.equal(continuation.color, outgoing[parentIndex].color);
        if (rows[next].oid === parent) {
          assert.equal(rows[next].lane, target);
          assert.equal(continuation.end, 'node');
          break;
        }
        assert.equal(continuation.end, 'bottom', `An edge must not terminate at unrelated ${rows[next].oid}`);
        assert.equal(continuation.to, target);
      }
    }
  }
  return rows;
}

test('linear commits share one lane and a root has no outgoing edge', () => {
  const commits = [commit('c', 'b'), commit('b', 'a'), commit('a')];
  const rows = verifyConnections(commits);
  assert.deepEqual(rows.map((row) => row.lane), [0, 0, 0]);
  assert.equal(layoutGitGraph(commits).width, 24);
  assert.deepEqual(plain(layoutGitGraph([])).rows, []);
});

test('merge paths preserve first-parent order and reconnect at their shared parent', () => {
  const rows = verifyConnections([commit('merge', 'main', 'feature'), commit('feature', 'root'), commit('main', 'root'), commit('root')]);
  assert.equal(rows[0].segments[0].to, 0);
  assert.equal(rows[0].segments[1].to, 1);
  assert.equal(rows[1].lane, 1);
  assert.equal(rows[2].lane, 0);
  assert.equal(rows[3].lane, 1);
});

test('multiple tips, octopus merges and duplicate/shared parents do not duplicate pending lanes', () => {
  verifyConnections([
    commit('tip', 'shared'), commit('octopus', 'left', 'right', 'other', 'left'),
    commit('left', 'shared'), commit('right', 'shared'), commit('other', 'shared'), commit('shared'),
  ]);
});

test('independent roots and released lanes leave passing branches in place', () => {
  const rows = verifyConnections([commit('a', 'root-a'), commit('root-b'), commit('b', 'root-a'), commit('root-a'), commit('root-c')]);
  assert.deepEqual(rows.map((row) => row.lane), [0, 1, 1, 0, 0]);
});

test('a missing parent remains a continuation and extending the window preserves every existing row', () => {
  const commits = [commit('m', 'a', 'b'), commit('a', 'root'), commit('b', 'root'), commit('root')];
  const complete = verifyConnections(commits);
  for (let count = 1; count < commits.length; count++) {
    const partial = verifyConnections(commits.slice(0, count));
    assert.deepEqual(partial, complete.slice(0, count));
    assert.ok(partial.at(-1).segments.some((edge) => edge.end === 'bottom'));
  }
  assert.deepEqual(plain(layoutGitGraph(commits)), plain(layoutGitGraph(commits)));
});

test('many parallel branches preserve all edges without shifting live lanes', () => {
  const commits = [commit('merge', ...Array.from({ length: 12 }, (_, i) => `branch-${i}`))];
  for (let i = 11; i >= 0; i--) commits.push(commit(`branch-${i}`, 'root'));
  commits.push(commit('root'));
  const rows = verifyConnections(commits);
  assert.equal(rows[1].lane, 11);
  assert.ok(layoutGitGraph(commits).width > 140);
  assert.ok(rows.every((row) => row.color >= 0 && row.color <= 3));
});

test('generated DAGs retain child-parent connections across every row boundary', () => {
  let seed = 21;
  const random = (max) => { seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0; return seed % max; };
  for (let trial = 0; trial < 25; trial++) {
    const commits = Array.from({ length: 30 }, (_, index) => {
      const parents = [];
      const remaining = 30 - index;
      for (let i = 0, count = random(Math.min(remaining, 4)); i < count; i++) parents.push(`c${index + 1 + random(remaining + 2)}`);
      return commit(`c${index}`, ...parents);
    });
    verifyConnections(commits);
  }
});

test('SVG paths meet on exact 24px row boundaries', () => {
  assert.equal(graphPath({ from: 0, to: 0, start: 'top', end: 'node' }), 'M12 0V12');
  assert.equal(graphPath({ from: 0, to: 0, start: 'node', end: 'bottom' }), 'M12 12V24');
  assert.equal(graphPath({ from: 0, to: 1, start: 'node', end: 'bottom' }), 'M12 12C12 18 24 18 24 24');
});

const loaderSource = stripTypeScriptTypes(await readFile(new URL('../src/gitHistory.ts', import.meta.url), 'utf8'));
const history = (name = 'repo', hasMore = true) => ({ root: name, commits: [], refs: [], head: { oid: null, refName: null }, hasMore });
function eventTarget() {
  const listeners = new Map();
  return {
    addEventListener(type, callback) {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(callback);
    },
    removeEventListener(type, callback) { listeners.get(type)?.delete(callback); },
    emit(type) { for (const callback of listeners.get(type) ?? []) callback(); },
    count: () => [...listeners.values()].reduce((sum, values) => sum + values.size, 0),
  };
}
async function setupLoader(t) {
  const timers = new Map();
  const window = {
    ...eventTarget(), setInterval: (callback) => { timers.set(callback, callback); return callback; },
    clearInterval: (id) => timers.delete(id),
  };
  const document = { ...eventTarget(), visibilityState: 'visible' };
  const module = new SourceTextModule(loaderSource, { context: createContext({ window, document }) });
  await module.link(() => { throw new Error('Unexpected runtime import'); });
  await module.evaluate();
  const requests = [];
  const states = [];
  const loader = module.namespace.createGitHistoryLoader((limit) => new Promise((resolve, reject) => {
    requests.push({ limit, resolve, reject });
  }), (state) => states.push(state));
  t.after(loader.stop);
  return {
    loader, requests, states, window, document, timers,
    get state() { return states.at(-1); },
    tick: () => { for (const callback of timers.values()) callback(); },
    async flush() { for (let i = 0; i < 10; i++) await Promise.resolve(); },
  };
}

test('history refresh skips overlapping periodic ticks and coalesces focus requests', async (t) => {
  const env = await setupLoader(t);
  assert.deepEqual(env.requests.map((request) => request.limit), [100]);
  env.tick(); env.tick();
  assert.equal(env.requests.length, 1);
  env.window.emit('focus'); env.window.emit('focus');
  env.requests[0].resolve(history());
  await env.flush();
  assert.equal(env.requests.length, 2);
  env.requests[1].resolve(history());
  await env.flush();
  assert.equal(env.state.loading, false);
});

test('collapse and visibility invalidate responses and re-expansion waits for the existing request', async (t) => {
  const env = await setupLoader(t);
  env.loader.setExpanded(false);
  env.tick();
  env.loader.setExpanded(true);
  assert.equal(env.requests.length, 1);
  env.requests[0].resolve(history('stale'));
  await env.flush();
  assert.equal(env.state.history, null);
  assert.equal(env.requests.length, 2);
  env.requests[1].resolve(history('current'));
  await env.flush();
  env.document.visibilityState = 'hidden'; env.document.emit('visibilitychange');
  env.tick(); env.window.emit('focus');
  assert.equal(env.requests.length, 2);
  env.document.visibilityState = 'visible'; env.document.emit('visibilitychange');
  assert.equal(env.requests.length, 3);
  env.requests[2].resolve(history('visible'));
  await env.flush();
  assert.equal(env.state.history.root, 'visible');
});

test('load more replaces the complete prefix and retries the same limit after failure', async (t) => {
  const env = await setupLoader(t);
  const first = history('first');
  env.requests[0].resolve(first); await env.flush();
  env.loader.loadMore();
  assert.equal(env.requests[1].limit, 200);
  assert.equal(env.state.history, first);
  env.loader.loadMore();
  assert.equal(env.requests.length, 2);
  env.requests[1].reject(new Error('read failed')); await env.flush();
  assert.match(env.state.error, /read failed/);
  assert.equal(env.state.history, first);
  env.loader.loadMore();
  assert.equal(env.requests[2].limit, 200);
  const next = history('new snapshot');
  env.requests[2].resolve(next); await env.flush();
  assert.equal(env.state.history, next);
  assert.equal(env.state.error, null);
});

test('history loading stops at the cap and releases listeners on disposal', async (t) => {
  const env = await setupLoader(t);
  for (let i = 0; i < 10; i++) {
    assert.equal(env.requests[i].limit, (i + 1) * 100);
    env.requests[i].resolve(history()); await env.flush();
    env.loader.loadMore();
  }
  assert.equal(env.requests.length, 10);
  env.loader.stop();
  assert.equal(env.timers.size + env.document.count() + env.window.count(), 0);
  env.loader.refresh();
  assert.equal(env.requests.length, 10);
});

test('unmounted panels never publish late results or queue follow-up work', async (t) => {
  const env = await setupLoader(t);
  env.loader.refresh();
  env.loader.stop();
  const count = env.states.length;
  env.requests[0].resolve(history('old folder')); await env.flush();
  assert.equal(env.states.length, count);
  assert.equal(env.requests.length, 1);
});
