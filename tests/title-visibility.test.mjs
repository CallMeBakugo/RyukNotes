import assert from 'node:assert/strict';
import test from 'node:test';

const { restoreTitleHeading, stripTitleHeading } = await import(
  new URL('../src/lib/editor/titleVisibility.ts', import.meta.url)
);

test('keeps a hidden title through source to rich to source and save', () => {
  const persisted = '# Note title\n\n## Something else\n\nBody\n';
  const body = '## Something else\n\nBody\n';

  const initialSource = stripTitleHeading(persisted, 'Note title', true);
  const rich = stripTitleHeading(
    restoreTitleHeading(initialSource.markdown, initialSource.hiddenTitle),
    'Note title',
    true
  );
  const toggledSource = stripTitleHeading(
    restoreTitleHeading(rich.markdown, rich.hiddenTitle),
    'Note title',
    true
  );

  assert.equal(rich.markdown, body);
  assert.equal(toggledSource.markdown, body);
  assert.equal(restoreTitleHeading(toggledSource.markdown, toggledSource.hiddenTitle), persisted);
});

test('keeps a title-only note visually empty and saves one title heading', () => {
  const initialSource = stripTitleHeading('# Note title\n', 'Note title', true);
  const rich = stripTitleHeading(
    restoreTitleHeading(initialSource.markdown, initialSource.hiddenTitle),
    'Note title',
    true
  );
  const saved = restoreTitleHeading(rich.markdown, rich.hiddenTitle);

  assert.equal(initialSource.markdown, '');
  assert.equal(rich.markdown, '');
  assert.equal((saved.match(/^# Note title$/gm) ?? []).length, 1);
});

test('replaces hidden title state when an unrelated note loads', () => {
  const markdown = '## Something else\n\nOther body\n';

  assert.deepEqual(stripTitleHeading(markdown, 'Another note', true), {
    markdown,
    hiddenTitle: null
  });
});

test('leaves the title visible when title hiding is disabled', () => {
  const persisted = '# Note title\n\n## Something else\n';

  assert.deepEqual(stripTitleHeading(persisted, 'Note title', false), {
    markdown: persisted,
    hiddenTitle: null
  });
});

test('reports the exact contiguous range removed without changing leading blank behavior', () => {
  for (const [markdown, expected, removedRange] of [
    ['# Note title\n\nBody', 'Body', { from: 0, to: 14 }],
    ['\n# Note title\nBody', '\nBody', { from: 1, to: 14 }],
    ['\n\n# Note title', '\n', { from: 1, to: 14 }],
    ['\n\n# Note title\n', '\n', { from: 1, to: 15 }],
    ['# Note title\r\n\r\nBody', 'Body', { from: 0, to: 16 }],
  ]) {
    const stripped = stripTitleHeading(markdown, 'Note title', true);
    assert.equal(stripped.markdown, expected);
    assert.deepEqual(stripped.removedRange, removedRange);
    assert.equal(markdown.slice(0, removedRange.from) + markdown.slice(removedRange.to), expected);
  }
});
