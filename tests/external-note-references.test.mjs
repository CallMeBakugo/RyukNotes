import assert from 'node:assert/strict';
import test from 'node:test';
import { externalNoteReferences } from '../src/lib/editor/externalNote.ts';

const ranges = raw => externalNoteReferences(raw).map(reference => ({
  ...reference, original: Buffer.from(raw).subarray(reference.from, reference.to).toString('utf8')
}));

test('finds Markdown attachment destinations without changing labels, titles, angles, or fragments', () => {
  const raw = '![Image|size=small](images/photo%20one.png "Caption")\n[PDF](<files/report (1).pdf#page=2> "Title")\n[CSV][ref]\n\n[ref]: files/list.csv\n';
  assert.deepEqual(ranges(raw).map(({ href, original, html }) => ({ href, original, html })), [
    { href: 'images/photo%20one.png', original: 'images/photo%20one.png', html: false },
    { href: 'files/report (1).pdf#page=2', original: 'files/report (1).pdf#page=2', html: false },
    { href: 'files/list.csv', original: 'files/list.csv', html: false }
  ]);
});

test('legacy space-containing destinations supported by the editor import without copying code examples', () => {
  const raw = '![Legacy](images/photo one.png "Caption")\n[File](files/report%20"Title")\n\n~~~md\n![Example](missing image.png)\n~~~\n\n`![Inline](missing image.png)`\n';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['images/photo one.png', 'files/report']);
});

test('unused and overridden reference definitions are not imported', () => {
  assert.deepEqual(ranges('A text-only note.\n\n[unused]: missing.png\n'), []);
  const raw = '![Image][photo]\n\n[photo]: images/real.png\n[PHOTO]: missing.png\n[unused]: also-missing.png\n';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['images/real.png']);
});

test('full, collapsed, and shortcut references select the first matching definition', () => {
  for (const use of ['![One][ Photo ]', '![photo][]', '![photo]', '[Photo][]', '[photo]']) {
    const raw = `${use}\n\n[PHOTO]: images/real.png\n[ photo ]: missing.png\n`;
    assert.deepEqual(ranges(raw).map(reference => reference.href), ['images/real.png'], use);
  }
});

test('reference labels in code, math, and image alt text do not authorize unused definitions', () => {
  const raw = '`![fake][hidden]`\n\n$[hidden]$\n\n![An [hidden] label](real.png)\n\n[hidden]: missing.png\n';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['real.png']);
});

test('UTF-8 byte offsets retain Unicode, BOM, CRLF, and untouched custom frontmatter', () => {
  const raw = '\uFEFF---\r\ncustom: "![metadata](do-not-copy.png)"\r\n---\r\n雪 📝 ![图片](资源/café%20雪.png)\r\n';
  assert.equal(ranges(raw).length, 1);
  assert.equal(ranges(raw)[0].original, '资源/café%20雪.png');
});

test('literal code, comments, closed display math, and HTML code do not become attachments', () => {
  const raw = '`![code](inline.png)`\n\n~~~~md\n![code](fenced.png)\n~~~~\n\n$$\n![math](math.png)\n$$\n\n<!-- <img src="comment.png"> -->\n\n<pre><code><img src="literal.png"></code></pre>\n\n![real](real.png)';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['real.png']);
});

test('inline math stays literal while escaped dollars and dollar-containing destinations remain links', () => {
  const raw = '$![math](math.png)$ \\$![real](real.png)\\$ ![price](images/$price$.png)';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['real.png', 'images/$price$.png']);
});

test('remote URLs, anchors, absolute paths, and linked Markdown notes stay unchanged', () => {
  const raw = '![remote](https://example.com/pic.png) [mail](mailto:test@example.com) [anchor](#section)\n![absolute](/private/file.png) [note](Related.MD#heading) [encoded](Related%2EMD)\n![data](data:image/png;base64,AAAA)';
  assert.deepEqual(ranges(raw), []);
});

test('HTML images, file links, and PDF embeds retain attribute byte ranges and decode entities', () => {
  const raw = '<img alt="a > b src=&quot;missing.png&quot;" src="images/a&amp;b.png"> <a href=files/report.pdf>File</a>\n\n<div data-pdf-src="files/view.pdf#page=2" data-pdf-name="View"></div>';
  assert.deepEqual(ranges(raw).map(({ href, original, html }) => ({ href, original, html })), [
    { href: 'images/a&b.png', original: 'images/a&amp;b.png', html: true },
    { href: 'files/report.pdf', original: 'files/report.pdf', html: true },
    { href: 'files/view.pdf#page=2', original: 'files/view.pdf#page=2', html: true }
  ]);
  assert.equal(ranges(raw)[2].pdf, true);
  assert.equal(ranges(raw)[0].pdf, false);
});

test('links inside image alt text are not copied as additional attachments', () => {
  const raw = '![An [example](missing.txt)](real.png)';
  assert.deepEqual(ranges(raw).map(reference => reference.href), ['real.png']);
});

test('escaped Markdown paths are decoded without consuming code examples or link titles', () => {
  const raw = '[Report](files/report\\(1\\).pdf "Other (unused.png)")';
  assert.equal(ranges(raw)[0].href, 'files/report(1).pdf');
  assert.equal(ranges(raw)[0].original, 'files/report\\(1\\).pdf');
});
