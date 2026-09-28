import assert from 'node:assert/strict';
import test from 'node:test';
import {
  resolveSearchTarget,
  visibleSearchTarget,
} from '../src/lib/editor/search-target.ts';

function targetIn(source, text, before = '', after = '', from = source.indexOf(text)) {
  return Object.freeze({ field: 'body', start: from, end: from + text.length, text, before, after });
}

function rangeIn(source, text) {
  const from = source.indexOf(text);
  return Object.freeze({ from, to: from + text.length });
}

test('recorded UTF-16 offsets require exact source text and surrounding context', () => {
  const source = 'prefix Needle suffix / prefix Needle suffix';
  const target = targetIn(source, 'Needle', 'prefix ', ' suffix', source.lastIndexOf('Needle'));
  assert.deepEqual(resolveSearchTarget(source, target), { from: target.start, to: target.end });
  assert.equal(resolveSearchTarget(source.toLowerCase(), target), null);
});

test('relocates a unique full anchor rather than the first occurrence of its text', () => {
  const original = 'prefix needle suffix';
  const target = targetIn(original, 'needle', 'prefix ', ' suffix');
  const changed = 'needle without context\nnew paragraph\n' + original;
  const resolved = resolveSearchTarget(changed, target);
  assert.deepEqual(resolved, { from: changed.lastIndexOf('needle'), to: changed.lastIndexOf('needle') + 6 });
  assert.equal(changed.slice(resolved.from, resolved.to), target.text);
});

test('does not choose an ambiguous relocated anchor or overlapping occurrence', () => {
  const original = 'prefix needle suffix';
  assert.equal(resolveSearchTarget('moved\n' + original + '\n' + original,
    targetIn(original, 'needle', 'prefix ', ' suffix')), null);
  assert.equal(resolveSearchTarget('x banana', targetIn('ana', 'ana')), null);
});

test('removed matches and changed context do not fall back to a remaining word', () => {
  const original = 'prefix needle suffix';
  const target = targetIn(original, 'needle', 'prefix ', ' suffix');
  for (const changed of [
    'prefix removed suffix',
    'prefix removed suffix / different needle context',
    'PREFIX needle suffix',
    'prefix needle SUFFIX',
  ]) assert.equal(resolveSearchTarget(changed, target), null);
});

test('an entire-field anchor can move only when its exact text is unique', () => {
  const target = targetIn('needle', 'needle');
  assert.deepEqual(resolveSearchTarget('new needle', target), { from: 4, to: 10 });
  assert.equal(resolveSearchTarget('new needle needle', target), null);
});

test('Unicode anchors preserve exact supplementary characters and source offsets', () => {
  const original = '😀 prefix 𐐀needle tail 😃';
  const target = targetIn(original, '𐐀needle', '😀 prefix ', ' tail 😃');
  const changed = 'intro 🧭\n' + original;
  assert.deepEqual(resolveSearchTarget(original, target), { from: target.start, to: target.end });
  assert.deepEqual(resolveSearchTarget(changed, target), {
    from: changed.indexOf('𐐀needle'), to: changed.indexOf('𐐀needle') + '𐐀needle'.length,
  });
  assert.equal(resolveSearchTarget(changed.replace('𐐀', '𐐨'), target), null);
});

test('invalid offsets and split-surrogate targets never produce a range', () => {
  const source = '😀 needle 😃';
  const target = targetIn(source, 'needle', '😀 ', ' 😃');
  for (const invalid of [
    { ...target, start: -1 },
    { ...target, start: 3.5 },
    { ...target, end: NaN },
    { ...target, end: target.end + 1 },
    { ...target, text: '', end: target.start },
    { ...target, before: '\ude00 ' },
    { ...target, after: ' \ud83d' },
    targetIn(source, '\ud83d', '', '', 0),
    targetIn(source, '\ude00', '', '', 1),
  ]) assert.equal(resolveSearchTarget(source, invalid), null);
  assert.equal(resolveSearchTarget('😀', targetIn('\ud83d', '\ud83d')), null);
});

test('CRLF anchors resolve against LF text without retaining old line offsets', () => {
  const original = 'lead\r\nprefix\r\nneedle\r\nsuffix\r\nend';
  const target = targetIn(original, 'needle\r\nsuffix', '\r\nprefix\r\n', '\r\nend');
  const normalized = original.replaceAll('\r\n', '\n');
  assert.deepEqual(resolveSearchTarget(original, target), { from: target.start, to: target.end });
  assert.deepEqual(resolveSearchTarget(normalized, target), {
    from: normalized.indexOf('needle'), to: normalized.indexOf('\nend'),
  });
  const lfTarget = targetIn(normalized, 'needle\nsuffix', '\nprefix\n', '\nend');
  assert.deepEqual(resolveSearchTarget(original, lfTarget), { from: target.start, to: target.end });
});

test('line-ending equivalence cannot make a duplicated relocated anchor unique', () => {
  const original = 'prefix\r\nneedle\r\nsuffix';
  const target = targetIn(original, 'needle', 'prefix\r\n', '\r\nsuffix');
  assert.equal(resolveSearchTarget('moved\n' + original + '\n' + original.replaceAll('\r\n', '\n'), target), null);
});

test('a target boundary inside CRLF is not relocated into an LF-only document', () => {
  const source = 'prefix\r\nneedle';
  const target = targetIn(source, '\nneedle', 'prefix\r');
  assert.equal(resolveSearchTarget(source.replaceAll('\r\n', '\n'), target), null);
});

test('visible source offsets remain unchanged without a matching hidden heading', () => {
  const body = '# Note title\n\nBody 😀 match';
  const match = rangeIn(body, '😀 match');
  for (const [title, hideTitle] of [['Note title', false], ['Different title', true], [undefined, true]]) {
    assert.deepEqual(visibleSearchTarget(body, title, hideTitle, match), { markdown: body, match, hiddenTitle: false });
  }
  const laterHeading = 'Intro\n\n# Note title\n\nBody match';
  assert.deepEqual(visibleSearchTarget(laterHeading, 'Note title', true, rangeIn(laterHeading, 'match')), {
    markdown: laterHeading, match: rangeIn(laterHeading, 'match'), hiddenTitle: false,
  });
});

test('hidden H1 offsets include its following blank line but preserve body matches', () => {
  const body = '# Note title\n\nBody 😀 match\n';
  const markdown = 'Body 😀 match\n';
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, '😀 match')), {
    markdown, match: rangeIn(markdown, '😀 match'), hiddenTitle: false,
  });
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, 'Note title')), {
    markdown, match: null, hiddenTitle: true,
  });
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, 'title\n\nBody')), {
    markdown, match: null, hiddenTitle: true,
  });
});

test('leading blanks retain their positions while later body matches shift', () => {
  const body = ' \n\n# Note title\n\nBody match';
  const markdown = ' \n\nBody match';
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, 'match')), {
    markdown, match: rangeIn(markdown, 'match'), hiddenTitle: false,
  });
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, { from: 0, to: 1 }), {
    markdown, match: { from: 0, to: 1 }, hiddenTitle: false,
  });
});

test('title-only notes hide their range, including leading-blank and missing-newline cases', () => {
  for (const body of ['# Note title', '# Note title\n', '\n\n# Note title', '\n\n# Note title\n']) {
    assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, 'Note title')), {
      markdown: body.startsWith('\n\n') ? '\n' : '', match: null, hiddenTitle: true,
    });
  }
});

test('heading normalization preserves the existing matching-title behavior', () => {
  const body = '  ### note—title  \n\nBody match';
  assert.deepEqual(visibleSearchTarget(body, 'Note_title', true, rangeIn(body, 'match')), {
    markdown: 'Body match', match: { from: 5, to: 10 }, hiddenTitle: false,
  });
});

test('CRLF and bare CR become CodeMirror LF offsets before title removal', () => {
  const body = '\r\n# Note title\r\n\r\nBody 😀\r\nnext match\rend';
  const markdown = '\nBody 😀\nnext match\nend';
  assert.deepEqual(visibleSearchTarget(body, 'Note title', true, rangeIn(body, '😀\r\nnext match')), {
    markdown, match: rangeIn(markdown, '😀\nnext match'), hiddenTitle: false,
  });
  const visibleHeading = body.replace(/\r\n?/g, '\n');
  assert.deepEqual(visibleSearchTarget(body, 'Note title', false, rangeIn(body, 'match\rend')), {
    markdown: visibleHeading, match: rangeIn(visibleHeading, 'match\nend'), hiddenTitle: false,
  });
});

test('visible ranges reject split surrogate pairs, split CRLF and invalid bounds', () => {
  const body = '😀\r\nword';
  for (const match of [
    { from: 0, to: 1 }, { from: 1, to: 2 }, { from: 3, to: 5 },
    { from: 0, to: 3 }, { from: -1, to: 1 }, { from: 2, to: 100 },
    { from: 4, to: 4 }, { from: 4.5, to: 6 },
  ]) assert.deepEqual(visibleSearchTarget(body, undefined, false, match), {
    markdown: '😀\nword', match: null, hiddenTitle: false,
  });
});
