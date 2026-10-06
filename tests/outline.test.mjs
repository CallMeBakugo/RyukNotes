import assert from 'node:assert/strict';
import test from 'node:test';
import { ChangeSet, EditorState } from '@codemirror/state';
import { Schema } from '@tiptap/pm/model';
import { Mapping, StepMap, Transform } from '@tiptap/pm/transform';
import {
  activeHeadingIndex,
  documentHeadings,
  mapDocumentHeadings,
  mapSourceHeadings,
  sourceHeadings,
} from '../src/lib/editor/outline.ts';

const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { group: 'block', content: 'inline*' },
    heading: { group: 'block', content: 'inline*', attrs: { level: { default: 1 } } },
    blockquote: { group: 'block', content: 'block+' },
    bullet_list: { group: 'block', content: 'list_item+' },
    list_item: { content: 'block+' },
    code_block: { group: 'block', content: 'text*', code: true },
    text: { group: 'inline' },
  },
  marks: { strong: {}, em: {} },
});
const paragraph = (text) => schema.node('paragraph', null, text ? schema.text(text) : null);
const heading = (text, level = 1) => schema.node('heading', { level }, text ? schema.text(text) : null);
const stateFor = (source) => EditorState.create({ doc: source });
const extract = (source) => sourceHeadings(stateFor(source));
const sourceEntry = (source, raw, level, text, offset = source.indexOf(raw)) => ({
  level, text, pos: offset, end: offset + raw.length,
});

test('document headings are semantic descendants with exact node extents and plain marked text', () => {
  const intro = paragraph('lead');
  const formatted = schema.node('heading', { level: 2 }, [
    schema.text('日本語😀', [schema.mark('strong')]), schema.text(' same', [schema.mark('em')]),
  ]);
  const quotedHeading = heading('same', 3);
  const quote = schema.node('blockquote', null, quotedHeading);
  const listedHeading = heading('same', 6);
  const list = schema.node('bullet_list', null, schema.node('list_item', null, [paragraph('item'), listedHeading]));
  const empty = heading('', 1);
  const code = schema.node('code_block', null, schema.text('# not a heading'));
  const doc = schema.node('doc', null, [intro, formatted, quote, list, empty, code, paragraph('# also not a heading')]);
  const formattedPos = intro.nodeSize;
  const quotePos = formattedPos + formatted.nodeSize;
  const listPos = quotePos + quote.nodeSize;
  const listedPos = listPos + 2 + paragraph('item').nodeSize;
  const emptyPos = listPos + list.nodeSize;
  assert.deepEqual(documentHeadings(doc), [
    { level: 2, text: '日本語😀 same', pos: formattedPos, end: formattedPos + formatted.nodeSize },
    { level: 3, text: 'same', pos: quotePos + 1, end: quotePos + 1 + quotedHeading.nodeSize },
    { level: 6, text: 'same', pos: listedPos, end: listedPos + listedHeading.nodeSize },
    { level: 1, text: '', pos: emptyPos, end: emptyPos + empty.nodeSize },
  ]);
});

test('source ATX levels use UTF-16 offsets and full heading extents', () => {
  const source = 'intro 😀\n\n' + [1, 2, 3, 4, 5, 6].map((level) => `${'#'.repeat(level)} 日本語😀 ${level}`).join('\n');
  assert.deepEqual(extract(source), [1, 2, 3, 4, 5, 6].map((level) => {
    const raw = `${'#'.repeat(level)} 日本語😀 ${level}`;
    return sourceEntry(source, raw, level, `日本語😀 ${level}`);
  }));
});

test('source labels render inline formatting, entities, links, image alt text and escaped markers', () => {
  const source = '# **Bold** *日本語😀* [Link](https://example.test) ![Alt *text*](image.png) `a_*b` &amp; \\*literal\\* <em>HTML</em> ==mark== H~2~ x^2^ ###';
  assert.deepEqual(extract(source), [{
    level: 1, text: 'Bold 日本語😀 Link Alt text a_*b & *literal* HTML mark H2 x2', pos: 0, end: source.length,
  }]);
});

test('reference links and images use their visible labels rather than Markdown syntax', () => {
  const source = '# [Reference][ref] and ![Image][img]\n\n[ref]: https://example.org\n[img]: image.png\n';
  assert.equal(extract(source)[0].text, 'Reference and Image');
});

test('empty labels and duplicate labels remain distinct navigable headings', () => {
  const source = '#\n## \n### ###\n# same\n# same';
  const raws = ['#', '## ', '### ###', '# same', '# same'];
  let pos = 0;
  const expected = raws.map((raw, index) => {
    const entry = sourceEntry(source, raw, [1, 2, 3, 1, 1][index], index < 3 ? '' : 'same', pos);
    pos += raw.length + 1;
    return entry;
  });
  assert.deepEqual(extract(source), expected);
});

test('fenced and indented code, escaped markers, and invalid ATX markers are excluded', () => {
  const source = [
    '# Real', '', '```md', '# fenced', 'Setext', '------', '```', '',
    '~~~', '## tilde fenced', '~~~', '', '    # indented', '    underline', '    =====', '',
    '\\# escaped', '', '#no-space', '', '####### too many', '',
    '> ```', '> # quoted code', '> ```', '', '## Last',
  ].join('\n');
  assert.deepEqual(extract(source), [
    sourceEntry(source, '# Real', 1, 'Real'), sourceEntry(source, '## Last', 2, 'Last'),
  ]);
});

test('closed display math never creates phantom ATX or Setext headings and preserves following offsets', () => {
  const source = '$$\nx\n---\n# math heading\n$$\n\n# Real\n\n$$\n## More math\n$$\n\n## Last';
  assert.deepEqual(extract(source), [
    sourceEntry(source, '# Real', 1, 'Real'), sourceEntry(source, '## Last', 2, 'Last'),
  ]);
});

test('math delimiters in fenced code and unclosed display math do not hide real Markdown headings', () => {
  const source = '```tex\n$$\n# code\n```\n\n# Real\n\n$$\n# Unclosed';
  assert.deepEqual(extract(source), [
    sourceEntry(source, '# Real', 1, 'Real'), sourceEntry(source, '# Unclosed', 1, 'Unclosed'),
  ]);
});

test('Setext headings and headings nested in quotes and lists retain semantic starts', () => {
  const source = [
    'Title 日本語😀', '====', '', 'Sub *title*', '----', '',
    '> ## Quote', '', '- ### Listed', '',
    '> Multi **line**', '> continued', '> ====', '',
    '- List setext', '  -----',
  ].join('\n');
  assert.deepEqual(extract(source), [
    sourceEntry(source, 'Title 日本語😀\n====', 1, 'Title 日本語😀'),
    sourceEntry(source, 'Sub *title*\n----', 2, 'Sub title'),
    sourceEntry(source, '## Quote', 2, 'Quote'),
    sourceEntry(source, '### Listed', 3, 'Listed'),
    sourceEntry(source, 'Multi **line**\n> continued\n> ====', 1, 'Multi line continued'),
    sourceEntry(source, 'List setext\n  -----', 2, 'List setext'),
  ]);
});

test('source extraction fully parses headings beyond an unconfigured or partial language tree', () => {
  const source = 'ordinary text\n\n'.repeat(2000) + '# Far heading';
  assert.deepEqual(extract(source), [sourceEntry(source, '# Far heading', 1, 'Far heading')]);
});

test('raw HTML blocks do not create Markdown outline entries or expose heading-looking text', () => {
  const source = '<h1>HTML title</h1>\n\n<div>\n# HTML block content\n</div>\n\n# Markdown';
  assert.deepEqual(extract(source), [sourceEntry(source, '# Markdown', 1, 'Markdown')]);
});

test('active heading uses the latest start, including boundaries and body content', () => {
  const headings = [
    { level: 1, text: 'same', pos: 5, end: 10 },
    { level: 2, text: 'same', pos: 20, end: 25 },
    { level: 1, text: '', pos: 35, end: 36 },
  ];
  for (const [pos, index] of [[0, -1], [4, -1], [5, 0], [10, 0], [19, 0], [20, 1], [34, 1], [35, 2], [100, 2]]) {
    assert.equal(activeHeadingIndex(headings, pos), index);
  }
  assert.equal(activeHeadingIndex([], 0), -1);
  assert.equal(activeHeadingIndex([{ ...headings[0], pos: 0 }], 0), 0);
});

test('source anchors map insertions before/at the heading start without absorbing end insertions', () => {
  const state = stateFor('lead\n# One\n\n## Two');
  const original = sourceHeadings(state);
  const before = state.update({ changes: { from: original[0].pos, insert: 'prefix\n' } });
  const shifted = mapSourceHeadings(original, before.changes.desc);
  assert.deepEqual(shifted, sourceHeadings(before.state));
  assert.deepEqual(original, sourceHeadings(state));
  const atEnd = before.state.update({ changes: { from: shifted[0].end, insert: '\nnew body' } });
  assert.deepEqual(mapSourceHeadings(shifted, atEnd.changes.desc), sourceHeadings(atEnd.state));
  const inLabel = atEnd.state.update({ changes: { from: shifted[0].pos + 2, insert: 'New ' } });
  const mapped = mapSourceHeadings(mapSourceHeadings(shifted, atEnd.changes.desc), inLabel.changes.desc);
  assert.equal(mapped[0].pos, sourceHeadings(inLabel.state)[0].pos);
  assert.equal(mapped[0].end, sourceHeadings(inLabel.state)[0].end);
  assert.equal(mapped[0].text, 'One'); // Labels refresh with the debounced extraction.
});

test('source anchors discard deletion/replacement instead of jumping to a duplicate or unrelated heading', () => {
  const state = stateFor('# same\n\n# same\n\n# last');
  const original = sourceHeadings(state);
  for (const insert of ['', '# unrelated']) {
    const transaction = state.update({ changes: { from: original[0].pos, to: original[0].end, insert } });
    const mapped = mapSourceHeadings(original, transaction.changes.desc);
    assert.deepEqual(mapped, sourceHeadings(transaction.state).slice(insert ? 1 : 0));
  }
  const markerDeleted = state.update({ changes: { from: 0, to: 1 } });
  assert.deepEqual(mapSourceHeadings(original, markerDeleted.changes.desc), sourceHeadings(markerDeleted.state));
  const covering = state.update({ changes: { from: 0, to: original[1].end, insert: '# replacement' } });
  assert.deepEqual(mapSourceHeadings(original, covering.changes.desc), sourceHeadings(covering.state).slice(1));
});

test('source mapping handles simultaneous and composed edits while preserving surviving anchors', () => {
  const state = stateFor('# first\n\n# second\n\n# third');
  const original = sourceHeadings(state);
  const first = state.update({ changes: [
    { from: 0, insert: 'intro\n\n' },
    { from: original[1].pos, to: original[1].end, insert: 'body' },
  ] });
  const second = first.state.update({ changes: { from: 0, insert: '😀\n' } });
  const composed = first.changes.desc.composeDesc(second.changes.desc);
  assert.deepEqual(mapSourceHeadings(original, composed), sourceHeadings(second.state));
  assert.deepEqual(mapSourceHeadings(original, ChangeSet.empty(state.doc.length)), original);
});

test('document anchors map transactions before debounce and exclude insertions at the end', () => {
  const doc = schema.node('doc', null, [paragraph('lead'), heading('one'), heading('two', 2)]);
  const original = documentHeadings(doc);
  const transform = new Transform(doc).insert(original[0].pos, paragraph('intro'));
  let mapped = mapDocumentHeadings(original, transform.mapping);
  assert.deepEqual(mapped, documentHeadings(transform.doc));
  const inserted = new Transform(transform.doc).insert(mapped[0].end, paragraph('body'));
  mapped = mapDocumentHeadings(mapped, inserted.mapping);
  assert.deepEqual(mapped, documentHeadings(inserted.doc));
  const edited = new Transform(inserted.doc).insert(mapped[0].pos + 1, schema.text('new '));
  const editedMapped = mapDocumentHeadings(mapped, edited.mapping);
  assert.equal(editedMapped[0].pos, documentHeadings(edited.doc)[0].pos);
  assert.equal(editedMapped[0].end, documentHeadings(edited.doc)[0].end);
  assert.deepEqual(original, documentHeadings(doc));
  assert.deepEqual(mapDocumentHeadings(original, new Mapping()), original);
});

test('document deletion and replacement never reuse an obsolete heading opening token', () => {
  const doc = schema.node('doc', null, [heading('same'), heading('same'), heading('last')]);
  const original = documentHeadings(doc);
  for (const replacement of [null, paragraph('body'), heading('unrelated')]) {
    const transform = new Transform(doc);
    if (replacement) transform.replaceWith(original[0].pos, original[0].end, replacement);
    else transform.delete(original[0].pos, original[0].end);
    const fresh = documentHeadings(transform.doc);
    assert.deepEqual(mapDocumentHeadings(original, transform.mapping), replacement?.type.name === 'heading' ? fresh.slice(1) : fresh);
  }
  const transform = new Transform(doc).insert(0, paragraph('intro'));
  const shifted = documentHeadings(transform.doc);
  transform.replaceWith(shifted[1].pos, shifted[1].end, paragraph('body'));
  assert.deepEqual(mapDocumentHeadings(original, transform.mapping), documentHeadings(transform.doc));
});

test('document mappings respect their selected step range', () => {
  const headings = [{ level: 1, text: 'one', pos: 5, end: 10 }];
  const mapping = new Mapping([new StepMap([0, 0, 3]), new StepMap([0, 0, 2])]);
  assert.deepEqual(mapDocumentHeadings(headings, mapping.slice(1)), [{ ...headings[0], pos: 7, end: 12 }]);
});

test('math delimiters in tilde fences cannot mask later real headings', () => {
  const source = '~~~tex\n$$\n$x$\n~~~\n\n# Real\n\n$$\nunclosed';
  assert.deepEqual(extract(source), [sourceEntry(source, '# Real', 1, 'Real')]);
});

test('masking display math retains list and quote context for following headings', () => {
  const source = '- Outer\n  - $$\n    x\n    ---\n    $$\n\n    ## Nested\n\n> $$\n> y\n> ---\n> $$\n> ## Quoted';
  assert.deepEqual(extract(source), [
    sourceEntry(source, '## Nested', 2, 'Nested'), sourceEntry(source, '## Quoted', 2, 'Quoted'),
  ]);
});
