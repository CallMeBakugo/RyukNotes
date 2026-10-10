import assert from 'node:assert/strict';
import test from 'node:test';
import { Schema } from '@tiptap/pm/model';
import { findDocumentMatches, findSourceMatches, closestSearchMatch } from '../src/lib/editor/search.ts';

const options = { matchCase: false, wholeWord: false };
const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { group: 'block', content: 'inline*' },
    text: { group: 'inline' },
    hard_break: { group: 'inline', inline: true },
    image: { group: 'inline', inline: true, atom: true },
  },
  marks: { bold: {}, italic: {} },
});
const text = (value, marks = []) => schema.text(value, marks.map((name) => schema.mark(name)));
const paragraph = (...children) => schema.node('paragraph', null, children);
const doc = (...children) => schema.node('doc', null, children);

test('rich phrases span adjacent text nodes regardless of formatting', () => {
  const document = doc(paragraph(text('A '), text('formatted', ['bold']), text(' phrase repeats. A formatted phrase repeats.')));
  const results = findDocumentMatches(document, 'formatted phrase', options);
  assert.deepEqual(results, [{ from: 3, to: 19 }, { from: 31, to: 47 }]);
  assert.deepEqual(results.map(({ from, to }) => document.textBetween(from, to)), ['formatted phrase', 'formatted phrase']);
});

test('rich search does not bridge paragraphs, hard breaks or inline atoms', () => {
  for (const document of [
    doc(paragraph(text('hello ')), paragraph(text('world'))),
    doc(paragraph(text('hello '), schema.node('hard_break'), text('world'))),
    doc(paragraph(text('hello '), schema.node('image'), text('world'))),
  ]) assert.deepEqual(findDocumentMatches(document, 'hello world', options), []);
});

test('Unicode case folding keeps original UTF-16 positions and supplementary letters intact', () => {
  const source = 'İ 😀 FoO 𐐀 Σςσ K';
  assert.deepEqual(findSourceMatches(source, 'foo', options), [{ from: 5, to: 8 }]);
  assert.deepEqual(findSourceMatches(source, '𐐨', options), [{ from: 9, to: 11 }]);
  assert.deepEqual(findSourceMatches(source, 'σ', options), [{ from: 12, to: 13 }, { from: 13, to: 14 }, { from: 14, to: 15 }]);
  assert.deepEqual(findSourceMatches(source, 'k', options), [{ from: 16, to: 17 }]);
  const document = doc(paragraph(text('İ 😀 '), text('FoO', ['italic'])));
  assert.deepEqual(findDocumentMatches(document, 'foo', options), [{ from: 6, to: 9 }]);
});

test('case-sensitive matches retain literal punctuation and overlapping occurrences', () => {
  assert.deepEqual(findSourceMatches('Foo foo FOO', 'foo', { ...options, matchCase: true }), [{ from: 4, to: 7 }]);
  assert.deepEqual(findSourceMatches('a.b a?b a.b', 'a.b', options), [{ from: 0, to: 3 }, { from: 8, to: 11 }]);
  assert.deepEqual(findSourceMatches('banana', 'ana', options), [{ from: 1, to: 4 }, { from: 3, to: 6 }]);
  assert.deepEqual(findSourceMatches('😀😀😀', '😀😀', options), [{ from: 0, to: 4 }, { from: 2, to: 6 }]);
  assert.deepEqual(findSourceMatches('anything', '', options), []);
});

test('whole-word boundaries include Unicode letters, marks, numbers and connectors', () => {
  const source = 'cat scatter cat_ cat2 écat caté cat\u0301 cat\u200d (cat) 😀cat😀';
  const results = findSourceMatches(source, 'cat', { ...options, wholeWord: true });
  assert.deepEqual(results.map(({ from }) => from), [0, source.indexOf('(cat)') + 1, source.indexOf('😀cat😀') + 2]);
  assert.deepEqual(findSourceMatches('中文 中文字', '中文', options), [{ from: 0, to: 2 }, { from: 3, to: 5 }]);
  assert.deepEqual(findSourceMatches('中文 中文字', '中文', { ...options, wholeWord: true }), [{ from: 0, to: 2 }]);
  const document = doc(paragraph(text('cat'), text('apult', ['bold']), text(' cat')));
  assert.deepEqual(findDocumentMatches(document, 'cat', { ...options, wholeWord: true }), [{ from: 10, to: 13 }]);
});

test('rescanning edited documents adds and removes occurrences without changing documents', () => {
  const before = doc(paragraph(text('word WORD')));
  const after = doc(paragraph(text('word WORD word')));
  assert.deepEqual(findDocumentMatches(before, 'word', options), [{ from: 1, to: 5 }, { from: 6, to: 10 }]);
  const results = findDocumentMatches(after, 'word', options);
  assert.deepEqual(results, [{ from: 1, to: 5 }, { from: 6, to: 10 }, { from: 11, to: 15 }]);
  assert.equal(closestSearchMatch(results, 6), 1);
  assert.equal(closestSearchMatch(results, 8), 2);
  assert.equal(closestSearchMatch(results, 100), 2);
  assert.equal(closestSearchMatch([], 100), 0);
  assert.equal(before.textContent, 'word WORD');
  assert.equal(after.textContent, 'word WORD word');
});


test('replace all takes each stretch of text once when matches overlap', async () => {
  const { replaceableMatches } = await import('../src/lib/editor/search.ts');
  const overlapping = findSourceMatches('aaaa b aa', 'aa', options);
  assert.deepEqual(overlapping.map(({ from }) => from), [0, 1, 2, 7]);
  assert.deepEqual(replaceableMatches(overlapping), [{ from: 0, to: 2 }, { from: 2, to: 4 }, { from: 7, to: 9 }]);
  assert.deepEqual(replaceableMatches([]), []);
});
