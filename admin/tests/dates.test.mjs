import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import ts from 'typescript';

const source = await readFile(new URL('../src/utils/dates.ts', import.meta.url), 'utf8');
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
});
const { formatDate, formatLastActivity } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`
);

test('zero activity is unknown regardless of timezone spelling', () => {
  for (const value of [
    '1970-01-01T00:00:00Z',
    '1970-01-01T00:00:00.000Z',
    '1970-01-01T08:00:00+08:00',
  ]) {
    assert.equal(formatLastActivity(value), '未记录');
    assert.equal(formatDate(value), '—');
  }
});

test('missing, invalid and pre-epoch dates are not rendered as activity', () => {
  for (const value of [undefined, null, '', 'invalid', '1969-12-31T23:59:59Z']) {
    assert.equal(formatLastActivity(value), '未记录');
    assert.equal(formatDate(value), '—');
  }
});

test('real activity is formatted in the viewer timezone without changing the instant', () => {
  const instant = '2026-10-24T16:30:15Z';
  const expected = new Date(instant).toLocaleString('zh-CN', { hour12: false });
  assert.equal(formatDate(instant), expected);
  assert.equal(formatLastActivity(instant), expected);
  assert.equal(formatLastActivity('2026-10-25T00:30:15+08:00'), expected);
});
