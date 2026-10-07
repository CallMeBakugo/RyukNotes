import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import MarkdownIt from 'markdown-it';
import { Schema } from '@tiptap/pm/model';
import { createServer, transformWithEsbuild } from 'vite';

const source = await readFile(
  new URL('../src/lib/editor/markdown.ts', import.meta.url),
  'utf8'
);
const { code } = await transformWithEsbuild(source, 'markdown.ts', {
  loader: 'ts',
  format: 'esm',
  target: 'esnext'
});
const { joinMarkdownBlocks, serializeCodeBlock, serializeInlineMarkdown } = await import(
  `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`
);

const vite = await createServer({
  configFile: false,
  server: { middlewareMode: true },
  appType: 'custom'
});
let editorMarkdown;
try {
  const { createEditorMarkdownParser } = await vite.ssrLoadModule(
    new URL('../src/lib/editor/markdownParser.ts', import.meta.url).pathname
  );
  editorMarkdown = createEditorMarkdownParser();
} finally {
  await vite.close();
}

// Link intentionally precedes formatting marks in the schema. This matches the
// mark order produced by the app and reproduces the original per-text-node bug.
const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { content: 'inline*', group: 'block' },
    text: { group: 'inline' },
    mathInline: { inline: true, group: 'inline', atom: true, attrs: { tex: { default: '' } } }
  },
  marks: {
    link: { attrs: { href: {} } },
    bold: {},
    italic: {},
    code: {},
    wikiLink: {
      attrs: {
        title: { default: null },
        aliased: { default: false }
      }
    }
  }
});

const markdown = new MarkdownIt({ html: true, linkify: false, breaks: false });
const bold = schema.marks.bold.create();
const italic = schema.marks.italic.create();
const link = schema.marks.link.create({ href: 'https://example.com' });

function paragraph(segments) {
  return schema.node(
    'paragraph',
    null,
    segments.map(({ text, marks }) => schema.text(text, marks))
  );
}

function assertMarkdown(segments, expected, expectedHtml) {
  const serialized = serializeInlineMarkdown(paragraph(segments));
  assert.equal(serialized, expected);
  assert.equal(markdown.render(serialized), `${expectedHtml}\n`);
}

test('keeps bold open when a link ends the marked span', () => {
  assertMarkdown(
    [
      { text: 'Bold ', marks: [bold] },
      { text: 'link', marks: [link, bold] }
    ],
    '**Bold [link](https://example.com)**',
    '<p><strong>Bold <a href="https://example.com">link</a></strong></p>'
  );
});

test('keeps bold open when a link starts the marked span', () => {
  assertMarkdown(
    [
      { text: 'link', marks: [link, bold] },
      { text: ' bold', marks: [bold] }
    ],
    '**[link](https://example.com) bold**',
    '<p><strong><a href="https://example.com">link</a> bold</strong></p>'
  );
});

test('keeps one bold span around a link in the middle', () => {
  assertMarkdown(
    [
      { text: 'Before ', marks: [bold] },
      { text: 'link', marks: [link, bold] },
      { text: ' after', marks: [bold] }
    ],
    '**Before [link](https://example.com) after**',
    '<p><strong>Before <a href="https://example.com">link</a> after</strong></p>'
  );
});

test('keeps nested bold and italic marks open across a link', () => {
  assertMarkdown(
    [
      { text: 'Nested ', marks: [bold, italic] },
      { text: 'link', marks: [link, bold, italic] },
      { text: ' marks', marks: [bold, italic] }
    ],
    '***Nested [link](https://example.com) marks***',
    '<p><em><strong>Nested <a href="https://example.com">link</a> marks</strong></em></p>'
  );
});

test('preserves wiki-link aliases inside a bold span', () => {
  const wikiLink = schema.marks.wikiLink.create({ title: 'Target note', aliased: true });
  const serialized = serializeInlineMarkdown(
    paragraph([
      { text: 'See ', marks: [bold] },
      { text: 'this note', marks: [wikiLink, bold] },
      { text: ' now', marks: [bold] }
    ])
  );

  assert.equal(serialized, '**See [[Target note|this note]] now**');
});

test('round-trips inline code backticks, delimiter padding and literal whitespace', () => {
  const codeMark = schema.marks.code.create();
  for (const text of [
    'a`b',
    'a``b',
    'a````b',
    '`',
    '``',
    '`edge`',
    ' a ',
    ' leading',
    'trailing ',
    ' ',
    '   ',
    '\tleading',
    '\u2003leading',
    '\t\u2003  interior\t\u2003 ',
    '\\$x$'
  ]) {
    const serialized = serializeInlineMarkdown(paragraph([{ text, marks: [codeMark] }]));
    const tokens = editorMarkdown.parseInline(serialized, {})[0].children;
    assert.deepEqual(tokens.map((token) => [token.type, token.content]), [['code_inline', text]], text);
    assert.equal(editorMarkdown.render(serialized).includes('data-math-inline'), false, text);
  }
  assert.equal(serializeInlineMarkdown(paragraph([{ text: '`', marks: [codeMark] }])), '`` ` ``');
  assert.equal(serializeInlineMarkdown(paragraph([{ text: ' a ', marks: [codeMark] }])), '`  a  `');
  assert.equal(serializeInlineMarkdown(paragraph([{ text: '   ', marks: [codeMark] }])), '`   `');
});

test('joins contiguous code across formatting changes with code taking precedence', () => {
  const codeMark = schema.marks.code.create();
  const serialized = serializeInlineMarkdown(paragraph([
    { text: '`a', marks: [codeMark, bold] },
    { text: '``', marks: [codeMark, italic] },
    { text: 'b`', marks: [codeMark] }
  ]));
  assert.equal(serialized, '``` `a``b` ```');
  assert.deepEqual(
    editorMarkdown.parseInline(serialized, {})[0].children.map((token) => [token.type, token.content]),
    [['code_inline', '`a``b`']]
  );
  assert.equal(editorMarkdown.render(serialized), '<p data-helix-code=""><code style="white-space: pre-wrap">`a``b`</code></p>\n');
});

test('keeps link syntax outside code and joins code across formatting changes within links', () => {
  const codeMark = schema.marks.code.create();
  const serialized = serializeInlineMarkdown(paragraph([
    { text: 'before ', marks: [] },
    { text: '`a', marks: [codeMark, link, bold] },
    { text: 'b`', marks: [codeMark, link, italic] },
    { text: ' after', marks: [] }
  ]));
  assert.equal(serialized, 'before [`` `ab` ``](https://example.com) after');
  assert.equal(
    editorMarkdown.render(serialized),
    '<p data-helix-code="">before <a href="https://example.com"><code style="white-space: pre-wrap">`ab`</code></a> after</p>\n'
  );
});

test('preserves formatting on non-code parts of a link', () => {
  const codeMark = schema.marks.code.create();
  const serialized = serializeInlineMarkdown(paragraph([
    { text: 'first', marks: [link, bold] },
    { text: 'code', marks: [link, codeMark] },
    { text: 'last', marks: [link, bold] }
  ]));
  assert.equal(
    editorMarkdown.render(serialized),
    '<p data-helix-code=""><strong><a href="https://example.com">first</a></strong><a href="https://example.com"><code style="white-space: pre-wrap">code</code></a><strong><a href="https://example.com">last</a></strong></p>\n'
  );
});

test('round-trips code blocks with embedded fences and every blank-line boundary', () => {
  for (const text of [
    '',
    'plain',
    '\nleading',
    'trailing\n',
    '\n\nleading\n\nmiddle\n\ntrailing\n\n',
    '\t  \n\u2003\n\n',
    '```\nembedded\n```\n',
    '``````\n~~~\n$not math$\n```'
  ]) {
    const serialized = serializeCodeBlock(text, 'js');
    const [fence] = markdown.parse(serialized, {});
    const expectedFence = '`'.repeat(Math.max(3, ...[...text.matchAll(/`+/g)].map((match) => match[0].length + 1)));
    assert.equal(serialized, `${expectedFence}js\n${text}\n${expectedFence}\n`, text);
    assert.equal(fence.type, 'fence', text);
    assert.equal(fence.info, 'js', text);
    // CommonMark includes the structural newline before the closing fence.
    assert.equal(fence.content, `${text}\n`, text);
    assert.equal(fence.content.slice(0, -1), text);
    assert.equal(editorMarkdown.render(serialized).includes('data-math-inline'), false, text);
  }
  assert.equal(serializeCodeBlock('code'), '```\ncode\n```\n');
});

test('escapes literal prose dollars without changing preceding literal backslashes', () => {
  for (const text of [
    '$x$',
    '$$',
    'cost $5 and $10',
    '\\$x$',
    '\\\\$x$',
    '\\\\\\$x$',
    'before \\$x\\$ after',
    '$a$ and $b$',
    'literal $with spaces$',
    'ordinary punctuation: a+b, c=d; e/f!'
  ]) {
    const serialized = serializeInlineMarkdown(paragraph([{ text, marks: [] }]));
    const tokens = editorMarkdown.parseInline(serialized, {})[0].children;
    assert.equal(tokens.every((token) => token.type === 'text'), true, text);
    assert.equal(tokens.map((token) => token.content).join(''), text);
    assert.equal(editorMarkdown.render(serialized).includes('data-math-inline'), false, text);
  }
  assert.equal(serializeInlineMarkdown(paragraph([{ text: '$x$', marks: [] }])), '\\$x\\$');
  assert.equal(
    serializeInlineMarkdown(paragraph([{ text: 'ordinary punctuation: a+b, c=d; e/f!', marks: [] }])),
    'ordinary punctuation: a+b, c=d; e/f!'
  );
});

test('keeps escaped Markdown dollars as prose through load, save and reload', () => {
  for (const source of ['\\$x\\$', 'price \\$5', '\\\\\\$x\\$', '\\\\\\\\\\$x\\$']) {
    const loaded = editorMarkdown.parseInline(source, {})[0].children;
    assert.equal(loaded.every((token) => token.type === 'text'), true);
    const text = loaded.map((token) => token.content).join('');
    const serialized = serializeInlineMarkdown(paragraph([{ text, marks: [] }]));
    const reloaded = editorMarkdown.parseInline(serialized, {})[0].children;
    assert.equal(reloaded.every((token) => token.type === 'text'), true);
    assert.equal(reloaded.map((token) => token.content).join(''), text);
  }
});

test('preserves real inline math TeX and falls back to existing encoded HTML when needed', () => {
  for (const tex of [
    'x^2',
    '\\frac{a}{b}',
    '',
    '   ',
    ' leading',
    'trailing ',
    '\tleading',
    'trailing\u2003',
    'a\nb',
    'a\rb',
    'x$y',
    '\\$',
    'trailing\\'
  ]) {
    const mathNode = schema.node('mathInline', { tex });
    const serialized = serializeInlineMarkdown(schema.node('paragraph', null, [mathNode]));
    const html = editorMarkdown.render(serialized);
    const stored = html.match(/data-math-inline="([^"]*)"/);
    assert.ok(stored, tex);
    assert.equal(decodeURIComponent(stored[1]), tex);
    if (tex === 'x^2' || tex === '\\frac{a}{b}') {
      assert.equal(serialized, `$${tex}$`);
    } else {
      assert.equal(serialized, `<span data-math-inline="${encodeURIComponent(tex)}" class="math-inline"></span>`);
    }
    const reloaded = schema.node('paragraph', null, [
      schema.node('mathInline', { tex: decodeURIComponent(stored[1]) })
    ]);
    assert.equal(serializeInlineMarkdown(reloaded), serialized);
  }
});

test('keeps literal dollars escaped within rich formatting and link labels', () => {
  const serialized = serializeInlineMarkdown(paragraph([
    { text: 'before ', marks: [bold] },
    { text: '$literal$', marks: [bold, link] },
    { text: ' after', marks: [bold] }
  ]));
  assert.equal(serialized, '**before [\\$literal\\$](https://example.com) after**');
  assert.equal(
    editorMarkdown.render(serialized),
    '<p><strong>before <a href="https://example.com">$literal$</a> after</strong></p>\n'
  );
});

test('uses encoded math storage at ambiguous dollar, digit and adjacent-atom boundaries', () => {
  const mathNode = schema.node('mathInline', { tex: 'x' });
  for (const children of [
    [mathNode, schema.text('2')],
    [mathNode, schema.text('$literal$')],
    [schema.text('$literal$'), mathNode],
    [mathNode, schema.node('mathInline', { tex: 'y' })]
  ]) {
    const serialized = serializeInlineMarkdown(schema.node('paragraph', null, children));
    const stored = [...editorMarkdown.render(serialized).matchAll(/data-math-inline="([^"]*)"/g)];
    const expected = children.filter((child) => child.type.name === 'mathInline').map((child) => child.attrs.tex);
    assert.deepEqual(stored.map((match) => decodeURIComponent(match[1])), expected);
    assert.equal(serialized.startsWith('$x$'), false);
  }
});

test('preserves trailing prose backslashes before real math atoms', () => {
  for (const text of ['\\', '\\\\', '\\\\\\']) {
    const serialized = serializeInlineMarkdown(schema.node('paragraph', null, [
      schema.text(text),
      schema.node('mathInline', { tex: 'x' })
    ]));
    const tokens = editorMarkdown.parseInline(serialized, {})[0].children;
    assert.equal(tokens[0].type, 'text');
    assert.equal(tokens[0].content, text);
    const stored = editorMarkdown.render(serialized).match(/data-math-inline="([^"]*)"/);
    assert.ok(stored);
    assert.equal(decodeURIComponent(stored[1]), 'x');
  }
});

test('splits contiguous code at link boundaries without losing code text or link targets', () => {
  const codeMark = schema.marks.code.create();
  const otherLink = schema.marks.link.create({ href: 'https://other.example' });
  const serialized = serializeInlineMarkdown(paragraph([
    { text: '`first', marks: [codeMark] },
    { text: 'second`', marks: [codeMark, link] },
    { text: 'third', marks: [codeMark, otherLink] },
    { text: 'last', marks: [codeMark] }
  ]));
  assert.equal(
    editorMarkdown.render(serialized),
    '<p data-helix-code=""><code style="white-space: pre-wrap">`first</code><a href="https://example.com"><code style="white-space: pre-wrap">second`</code></a><a href="https://other.example"><code style="white-space: pre-wrap">third</code></a><code style="white-space: pre-wrap">last</code></p>\n'
  );
});

const block = (text) => ({ text, isImage: false });
const image = { text: '![x](a.png)\n', isImage: true };
const emptyParagraph = { text: '', isImage: false, isEmpty: true };

test('joined blocks are separated by exactly one blank line, including after lists', () => {
  assert.equal(joinMarkdownBlocks([block('one\n'), block('two\n')]), 'one\n\ntwo\n');
  assert.equal(joinMarkdownBlocks([block('- a\n- b\n\n'), block('text\n')]), '- a\n- b\n\ntext\n');
  assert.equal(joinMarkdownBlocks([block('- a\n\n'), block('1. b\n\n'), block('text\n')]), '- a\n\n1. b\n\ntext\n');
  assert.equal(joinMarkdownBlocks([block('text\n'), block('- a\n\n')]), 'text\n\n- a\n');
});

test('empty paragraphs stay as marker lines between blocks and are dropped at the end', () => {
  assert.equal(
    joinMarkdownBlocks([block('a\n'), emptyParagraph, emptyParagraph, block('b\n'), emptyParagraph, emptyParagraph]),
    'a\n\n<!-- -->\n<!-- -->\nb\n'
  );
  assert.equal(joinMarkdownBlocks([emptyParagraph]), '\n');
});

test('an empty paragraph before an image is written as a blank line that parses back to it', () => {
  const gap = '<p></p>\n<p><img';
  for (const before of [block('# T\n'), block('para\n'), block('- a\n\n'), block('```\ncode\n```\n')]) {
    const joined = joinMarkdownBlocks([before, emptyParagraph, image, block('text\n')]);
    assert.equal(joined, `${before.text.replace(/\n*$/, '\n')}\n![x](a.png)\n\ntext\n`, before.text);
    assert.doesNotMatch(joined, /<!--/);
    assert.ok(editorMarkdown.render(joined).includes(gap), joined);
    // Saving what was just loaded must not change it.
    assert.equal(joinMarkdownBlocks([before, emptyParagraph, image, block('text\n')]), joined);
  }
  // A second empty paragraph keeps its marker and still leaves a blank line before the image.
  const twice = joinMarkdownBlocks([block('para\n'), emptyParagraph, emptyParagraph, image]);
  assert.equal(twice, 'para\n\n<!-- -->\n\n![x](a.png)\n');
  assert.equal(editorMarkdown.render(twice).match(/<p><\/p>/g)?.length, 2);
});

test('an image placed directly after a block stays directly after it', () => {
  const joined = joinMarkdownBlocks([block('# T\n'), image]);
  assert.equal(joined, '# T\n![x](a.png)\n');
  assert.doesNotMatch(editorMarkdown.render(joined), /<p><\/p>/);
});

test('joining never collapses blank lines inside code blocks', () => {
  const code = serializeCodeBlock('a\n\n\n\nb');
  assert.equal(joinMarkdownBlocks([block(code), block('after\n')]), code + '\nafter\n');
  assert.ok(joinMarkdownBlocks([block(code), block('after\n')]).includes('a\n\n\n\nb'));
});
