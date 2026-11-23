import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { stripTypeScriptTypes } from 'node:module';
import test from 'node:test';
import { createContext, SourceTextModule } from 'node:vm';

const source = stripTypeScriptTypes(
  await readFile(new URL('../src/format.ts', import.meta.url), 'utf8'),
);
const context = createContext({});
const module = new SourceTextModule(source, { context });
await module.link(() => {});
await module.evaluate();
const { formatClock, formatModel, isFreshTurn } = module.namespace;

test('formatModel shortens Claude ids and leaves other providers alone', () => {
  assert.equal(formatModel('claude-sonnet-4-5-20250929'), 'Sonnet 4.5');
  assert.equal(formatModel('claude-3-5-haiku-20241022'), 'Haiku 3.5');
  assert.equal(formatModel('claude-opus-4-1'), 'Opus 4.1');
  assert.equal(formatModel('claude-3-sonnet'), 'Sonnet 3');
  assert.equal(formatModel('deepseek-v4.1'), 'deepseek-v4.1');
  assert.equal(formatModel(null), null);
  assert.equal(formatModel('  '), null);
  assert.equal(formatModel('<synthetic>'), null);
});

test('formatClock renders local HH:mm and tolerates unusable timestamps', () => {
  const at = new Date(2026, 10, 23, 9, 5, 0);
  assert.equal(formatClock(at.toISOString()), '09:05');
  assert.equal(formatClock(null), '');
  assert.equal(formatClock(''), '');
  assert.equal(formatClock('not-a-date'), '');
});

test('isFreshTurn animates live turns and recent timestamps only', () => {
  const now = Date.parse('2026-11-23T10:00:00Z');
  assert.equal(isFreshTurn(null, true, now), true);
  assert.equal(isFreshTurn(null, false, now), false);
  assert.equal(isFreshTurn('2026-11-23T09:59:55Z', false, now), true);
  assert.equal(isFreshTurn('2026-11-23T09:00:00Z', false, now), false);
  assert.equal(isFreshTurn('2026-11-23T09:00:00Z', true, now), true);
});
