import assert from 'node:assert/strict';
import test from 'node:test';
import { getSchema } from '@tiptap/core';
import StarterKit from '@tiptap/starter-kit';
import Image from '@tiptap/extension-image';
import { DOMSerializer } from '@tiptap/pm/model';
import {
  expandImageRange,
  parseImgGridHeader,
  reorderTarget,
  serializeImgGrid,
  serializeImgGridHeader,
  splitTrailingCloser
} from '../src/lib/editor/imageGrid.ts';
import { ImageGrid } from '../src/lib/editor/extensions/imageGrid.ts';
import { createEditorMarkdownParser } from '../src/lib/editor/markdownParser.ts';
import { joinMarkdownBlocks, serializeInlineMarkdown } from '../src/lib/editor/markdown.ts';

const parser = createEditorMarkdownParser();
const SizedImage = Image.extend({
  addAttributes() {
    return { ...this.parent?.(), size: { default: 'full' } };
  }
});
const schema = getSchema([StarterKit, SizedImage.configure({ inline: true }), ImageGrid]);

/** Grid blocks as the editor would load them: cols/layout plus each image's src, alt and size. */
function parseGrids(markdown) {
  const grids = [];
  let current = null;
  for (const token of parser.parse(markdown, {})) {
    if (token.type === 'helix_img_grid_open') {
      current = { cols: Number(token.attrGet('data-cols')), layout: token.attrGet('data-layout'), images: [] };
    } else if (token.type === 'helix_img_grid_close') {
      grids.push(current);
      current = null;
    } else if (current && token.type === 'inline') {
      for (const child of token.children) {
        if (child.type !== 'image') continue;
        // The editor splits `|size=` off the alt text when it loads images (normalizeImageHtml).
        const alt = child.content;
        const size = /\|size=(small|medium|full)$/.exec(alt);
        current.images.push({ src: child.attrGet('src'), alt: size ? alt.slice(0, size.index) : alt, size: size ? size[1] : 'full', title: child.attrGet('title') });
      }
    }
  }
  return grids;
}

/** Save a parsed grid the way Editor.svelte does: inline image Markdown wrapped by serializeImgGrid. */
function saveGrid(grid) {
  const node = schema.nodes.imageGrid.create(
    { cols: grid.cols, layout: grid.layout },
    grid.images.map((image) => schema.nodes.image.create(image))
  );
  return serializeImgGrid(node.attrs, serializeInlineMarkdown(node, decodeURI));
}

const roundTrip = (markdown) => parseGrids(markdown).map(saveGrid).join('');
const rendersGrid = (markdown) => parser.render(markdown).includes('data-img-grid');

test('parses canonical headers and their aliases', () => {
  assert.deepEqual(parseImgGridHeader(':::img-grid'), { cols: 2, layout: 'grid' });
  assert.deepEqual(parseImgGridHeader(':::img-grid cols=3'), { cols: 3, layout: 'grid' });
  assert.deepEqual(parseImgGridHeader(':::img-grid cols=4 masonry'), { cols: 4, layout: 'masonry' });
  assert.deepEqual(parseImgGridHeader(':::img-grid layout=masonry cols=3'), { cols: 3, layout: 'masonry' });
  assert.deepEqual(parseImgGridHeader(':::img-grid grid  '), { cols: 2, layout: 'grid' });
});

test('rejects headers it would have to rewrite, so they stay literal', () => {
  for (const line of [
    ':::img-grid cols=1',
    ':::img-grid cols=5',
    ':::img-grid cols=99',
    ':::img-grid cols=3 cols=4',
    ':::img-grid masonry grid',
    ':::img-grid gap=4',
    ':::img-gridx',
    '::: img-grid',
    '```img-grid'
  ]) {
    assert.equal(parseImgGridHeader(line), null, line);
  }
});

test('saves canonical headers', () => {
  assert.equal(serializeImgGridHeader({ cols: 2, layout: 'grid' }), ':::img-grid');
  assert.equal(serializeImgGridHeader({ cols: 3, layout: 'masonry' }), ':::img-grid cols=3 masonry');
  assert.equal(serializeImgGridHeader({ cols: 2, layout: 'masonry' }), ':::img-grid masonry');
  assert.equal(serializeImgGrid({ cols: 2, layout: 'grid' }, ''), '', 'an empty grid saves nothing');
});

test('round-trips grids byte for byte', () => {
  for (const markdown of [
    ':::img-grid\n![](a.png)\n![](b.png)\n:::\n',
    ':::img-grid cols=3 masonry\n![One](photos/one.jpg)\n![Two](photos/two.jpg)\n![Three](photos/three.jpg)\n:::\n',
    ':::img-grid cols=4\n![](a.png)\n:::\n'
  ]) {
    assert.equal(roundTrip(markdown), markdown);
  }
});

test('keeps small and medium sizes of grid images (Yuri: sizes became full on reload)', () => {
  const markdown = ':::img-grid\n![a|size=small](a.png)\n![b|size=medium](b.png)\n![c](c.png)\n:::\n';
  assert.deepEqual(parseGrids(markdown)[0].images.map((image) => image.size), ['small', 'medium', 'full']);
  assert.equal(roundTrip(markdown), markdown);
});

test('leaves a grid example inside a fenced code block literal (Yuri: fences were converted)', () => {
  for (const markdown of [
    '```md\n:::img-grid\n![](a.png)\n:::\n```\n',
    '~~~\n:::img-grid cols=3\n![](a.png)\n![](b.png)\n:::\n~~~\n',
    '- item\n\n    ```\n    :::img-grid\n    ![](a.png)\n    :::\n    ```\n'
  ]) {
    assert.equal(rendersGrid(markdown), false, markdown);
    assert.match(parser.render(markdown), /:::img-grid/);
  }
});

test('never partially converts: captions, links, text and gaps keep the whole block as Markdown', () => {
  for (const markdown of [
    ':::img-grid\n![](a.png)\nA caption\n![](b.png)\n:::\n',
    ':::img-grid\n![](a.png)\n[![](b.png)](https://example.com)\n:::\n',
    ':::img-grid\n![](a.png) and text\n:::\n',
    ':::img-grid\n![](a.png)\n\n![](b.png)\n:::\n',
    ':::img-grid\n![](a.png)\n![](b.png)\n',
    ':::img-grid\n:::\n',
    ':::img-grid cols=9\n![](a.png)\n:::\n'
  ]) {
    assert.equal(rendersGrid(markdown), false, markdown);
    const html = parser.render(markdown);
    for (const source of markdown.match(/\]\(([^)]+)\)/g) ?? []) {
      const file = source.slice(2, -1);
      if (file.endsWith('.png')) assert.ok(html.includes(`src="${file}"`), `${file} kept in ${JSON.stringify(markdown)}`);
    }
    if (markdown.includes('A caption')) assert.match(html, /A caption/);
    if (markdown.includes('https://example.com')) assert.match(html, /href="https:\/\/example.com"/);
  }
});

test('parses titles and angle-bracket destinations like any other image (Yuri: title encoded into the URL)', () => {
  const [grid] = parseGrids(':::img-grid\n![cat](cat.png "Photo (2024)")\n![](<my file.png>)\n![](<a (1).png>)\n:::\n');
  assert.deepEqual(grid.images.map((image) => image.src), ['cat.png', 'my%20file.png', 'a%20(1).png']);
  // Grid images follow the same title policy as every other image; either way the image must survive.
  const saved = roundTrip(':::img-grid\n![cat](cat.png "Photo (2024)")\n![](b.png)\n:::\n');
  assert.match(saved, /^:::img-grid\n!\[cat\]\(cat\.png[ )]/);
  assert.deepEqual(parseGrids(saved)[0].images.map((image) => image.src), ['cat.png', 'b.png']);
  assert.equal(roundTrip(saved), saved);
});

test('ampersands and quotes in alt text and paths do not grow on repeated saves (Yuri: & re-escaped)', () => {
  let markdown = ':::img-grid\n![Tom & Jerry](a&b.png)\n![](c.png)\n:::\n';
  for (let i = 0; i < 3; i++) markdown = roundTrip(markdown);
  assert.equal(markdown, ':::img-grid\n![Tom & Jerry](a&b.png)\n![](c.png)\n:::\n');
});

test('accepts a closer trailing the last image and saves it on its own line', () => {
  assert.equal(splitTrailingCloser('![](b.png) :::'), '![](b.png)');
  assert.equal(splitTrailingCloser('![](b.png):::'), null);
  const markdown = ':::img-grid\n![](a.png)\n![](b.png) :::\nAfter\n';
  assert.equal(rendersGrid(markdown), true);
  assert.match(parser.render(markdown), /<p>After<\/p>/);
  assert.equal(roundTrip(markdown), ':::img-grid\n![](a.png)\n![](b.png)\n:::\n');
});

test('grids nest in lists and quotes', () => {
  assert.equal(rendersGrid('- item\n\n    :::img-grid\n    ![](a.png)\n    ![](b.png)\n    :::\n'), true);
  assert.equal(rendersGrid('> :::img-grid\n> ![](a.png)\n> :::\n'), true);
});

test('saved notes reload with the grid intact next to text and images', () => {
  const grid = saveGrid({ cols: 3, layout: 'grid', images: [{ src: 'a.png', alt: '', size: 'full' }, { src: 'b.png', alt: '', size: 'full' }] });
  const note = joinMarkdownBlocks([
    { text: 'Intro\n', isImage: false },
    { text: grid, isImage: false },
    { text: '![](c.png)\n', isImage: true },
    { text: 'Outro\n', isImage: false }
  ]);
  assert.equal(note, 'Intro\n\n:::img-grid cols=3\n![](a.png)\n![](b.png)\n:::\n![](c.png)\n\nOutro\n');
  const html = parser.render(note);
  assert.equal(html.match(/data-img-grid/g)?.length, 1);
  assert.match(html, /<p><img src="c.png" alt=""><\/p>/);
  assert.equal(roundTrip(note), grid);
});

/** Just enough DOM for ProseMirror's DOMSerializer. */
const fakeDocument = {
  createElement(tag) {
    return {
      tag,
      attributes: {},
      children: [],
      setAttribute(name, value) { this.attributes[name] = value; },
      appendChild(child) { this.children.push(child); return child; }
    };
  },
  createTextNode(text) { return { text }; }
};

test('renderHTML marks grids with data-img-grid so copy and paste keeps the layout (Yuri: lost on paste)', () => {
  const node = schema.nodes.imageGrid.create({ cols: 3, layout: 'masonry' }, [
    schema.nodes.image.create({ src: 'a.png' }),
    schema.nodes.image.create({ src: 'b.png' })
  ]);
  const dom = DOMSerializer.fromSchema(schema).serializeNode(node, { document: fakeDocument });
  assert.equal(dom.tag, 'div');
  assert.equal(dom.attributes['data-img-grid'], '');
  assert.equal(dom.attributes['data-cols'], '3');
  assert.equal(dom.attributes['data-layout'], 'masonry');
  assert.deepEqual(dom.children.map((child) => child.tag), ['img', 'img']);
  // ...and that is exactly what the parse rule matches.
  assert.deepEqual(schema.nodes.imageGrid.spec.parseDOM.map((rule) => rule.tag), ['div[data-img-grid]']);
});

test('groups neighboring image blocks only when there are at least two images', () => {
  const blocks = [
    { isImage: false, imageCount: 0 },
    { isImage: true, imageCount: 1 },
    { isImage: true, imageCount: 2 },
    { isImage: false, imageCount: 0 },
    { isImage: true, imageCount: 1 }
  ];
  assert.deepEqual(expandImageRange(blocks, 1), { start: 1, end: 2, total: 3 });
  assert.equal(expandImageRange(blocks, 4), null);
  assert.equal(expandImageRange(blocks, 0), null);
});

test('groups images a blank line apart, which load with an empty paragraph between them', () => {
  const text = { isImage: false, imageCount: 0 };
  const image = { isImage: true, imageCount: 1 };
  const empty = { isImage: false, imageCount: 0, isEmpty: true };
  // text, blank line, image, blank line, image, blank line, image, text
  const blocks = [text, empty, image, empty, image, empty, image, text];
  for (const target of [2, 4, 6]) {
    // The empty paragraph above the first image goes too, or it would be saved as a marker above the grid.
    assert.deepEqual(expandImageRange(blocks, target), { start: 1, end: 6, total: 3 });
  }
  // Two empty paragraphs are deliberate spacing and are not bridged.
  assert.equal(expandImageRange([image, empty, empty, image], 0), null);
  // An empty paragraph that does not lead to another image stays out of the range.
  assert.deepEqual(expandImageRange([image, image, empty, text], 0), { start: 0, end: 1, total: 2 });
  assert.deepEqual(expandImageRange([empty, image, empty, image], 1), { start: 0, end: 3, total: 2 });
  assert.deepEqual(expandImageRange([image, empty, image], 2), { start: 0, end: 2, total: 2 });
});

test('drag reorder targets account for the removed item', () => {
  assert.equal(reorderTarget(0, 3, 3), 2, 'drop at the end');
  assert.equal(reorderTarget(2, 0, 3), 0, 'drop at the start');
  assert.equal(reorderTarget(1, 1, 3), null, 'drop on itself');
  assert.equal(reorderTarget(1, 2, 3), null, 'drop just after itself');
  assert.equal(reorderTarget(3, 0, 3), null, 'stale index');
});
