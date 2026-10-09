import assert from 'node:assert/strict';
import test from 'node:test';
import { getSchema } from '@tiptap/core';
import StarterKit from '@tiptap/starter-kit';
import Image from '@tiptap/extension-image';
import { createEditorMarkdownParser } from '../src/lib/editor/markdownParser.ts';
import { serializeImageTitle, serializeInlineMarkdown } from '../src/lib/editor/markdown.ts';

const parser = createEditorMarkdownParser();
const schema = getSchema([StarterKit, Image.configure({ inline: true })]);

const save = (attrs) => serializeInlineMarkdown(schema.nodes.paragraph.create(null, schema.nodes.image.create(attrs)));
const images = (markdown) => parser.parseInline(markdown, {})[0].children.filter((token) => token.type === 'image');
const load = (markdown) => {
  const [image] = images(markdown);
  return { src: image.attrGet('src'), alt: image.content, title: image.attrGet('title') };
};

const titles = [
  'Photo (2024)',
  'Tom & Jerry',
  '&amp; is an entity',
  '&#38; and &copy;',
  'Say "hi"',
  "it's",
  'C:\\Users\\shawn',
  'ends with a backslash \\',
  '\\" tricky',
  '[brackets] and *stars* and `ticks`',
  '<b>not html</b>',
  '$5 and $$',
  'unicode 🎉 ✓'
];

test('image titles round-trip exactly through save and load', () => {
  for (const title of titles) {
    const markdown = save({ src: 'cat.png', alt: 'cat', title });
    assert.deepEqual(load(markdown), { src: 'cat.png', alt: 'cat', title }, markdown);
  }
});

test('repeated saves do not change a title (no growing escapes)', () => {
  for (const title of titles) {
    const first = save({ src: 'cat.png', alt: '', title });
    let markdown = first;
    for (let i = 0; i < 3; i++) markdown = save({ src: 'cat.png', alt: '', title: load(markdown).title });
    assert.equal(markdown, first, title);
  }
});

test('titles are written in readable CommonMark form', () => {
  assert.equal(save({ src: 'cat.png', alt: 'cat', title: 'Photo (2024)' }), '![cat](cat.png "Photo (2024)")');
  assert.equal(save({ src: 'cat.png', alt: '', title: 'Tom & Jerry' }), '![](cat.png "Tom & Jerry")');
  assert.equal(save({ src: 'cat.png', alt: '', title: 'Say "hi"' }), '![](cat.png "Say \\"hi\\"")');
  assert.equal(save({ src: 'cat.png', alt: '', title: '&amp;' }), '![](cat.png "\\&amp;")');
  assert.equal(save({ src: 'cat.png', alt: '' }), '![](cat.png)');
  assert.equal(serializeImageTitle(''), '');
  assert.equal(serializeImageTitle(null), '');
});

test('titles written by other apps load as hover text', () => {
  assert.equal(load('![cat](cat.png "Photo (2024)")').title, 'Photo (2024)');
  assert.equal(load("![cat](cat.png 'single')").title, 'single');
  assert.equal(load('![cat](cat.png (parens))').title, 'parens');
  assert.equal(load('![cat](<my cat.png> "angle")').src, 'my%20cat.png');
  assert.equal(load('![cat](<my cat.png> "angle")').title, 'angle');
});

test('legacy space and %20 destinations keep their image titles', () => {
  assert.deepEqual(load('![a](my file.png "Hover")'), { src: 'my%20file.png', alt: 'a', title: 'Hover' });
  assert.deepEqual(load("![a](my file.png 'Hover')"), { src: 'my%20file.png', alt: 'a', title: 'Hover' });
  assert.deepEqual(load('![a](my%20file.png%20"Hover")'), { src: 'my%20file.png', alt: 'a', title: 'Hover' });
  assert.deepEqual(load('![a](<my file.png>%20"Hover")'), { src: 'my%20file.png', alt: 'a', title: 'Hover' });
  assert.equal(load('![a](my file.png)').title, null);
});

test('link titles are still not kept', () => {
  const html = parser.render('[a](x.md "Link title") ![b](y.png "Image title")');
  assert.doesNotMatch(html, /<a [^>]*title=/);
  assert.match(html, /<img src="y.png" alt="b" title="Image title">/);
});
