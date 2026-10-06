import assert from 'node:assert/strict';
import test from 'node:test';
import { createEditorMarkdownParser, displayMathRanges } from '../src/lib/editor/markdownParser.ts';

const parser = createEditorMarkdownParser();
const payload = '$x$\n$$\na+b\n$$\n[[Note]]\n[label](path with spaces)\n![image](<image path> "title")\n- [x] task\n<!-- -->\n<div data-pdf-src="a.pdf" data-pdf-name="A"></div>';
const escapedCode = (html) => html.match(/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/)?.[1];
const unescape = (html) => html.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&amp;/g, '&');
const mathPayloads = (html) => [...html.matchAll(/data-math-(?:inline|block)="([^"]*)"/g)].map(match => decodeURIComponent(unescape(match[1])));

for (const [name, opening, closing] of [
  ['backtick', '```text', '```'], ['tilde', '~~~text', '~~~'],
  ['long backtick', '````text', '````'], ['indented fence', '   ~~~text', '   ~~~'],
]) {
  test(`${name} fences preserve all literal syntax`, () => {
    const html = parser.render(`${opening}\n${payload}\n${closing}`, { wikiLinkTitles: [{ path: 'Note.md', title: 'Note' }] });
    assert.equal(unescape(escapedCode(html)), payload);
    assert.equal(mathPayloads(html).length, 0);
    assert.doesNotMatch(html, /<span[^>]*data-wiki-link|<li[^>]*data-checked|<div[^>]*data-secret-block/);
  });
}

test('shorter and mismatched fences remain code; following math still renders', () => {
  const code = '```\n~~~\n$x$';
  const html = parser.render('````text\n' + code + '\n````\n\nActual $y$.');
  assert.equal(unescape(escapedCode(html)), code);
  assert.deepEqual(mathPayloads(html), ['y']);
});

test('fences interrupt prose without needing a blank line', () => {
  const html = parser.render('Prose\n```text\n$x$\n```\nMath $y$.');
  assert.equal(unescape(escapedCode(html)), '$x$');
  assert.deepEqual(mathPayloads(html), ['y']);
});

test('quoted and listed fences protect their contents', () => {
  const quoted = parser.render('> ~~~text\n> $x$\n> [[Note]]\n> ~~~', { wikiLinkTitles: [] });
  assert.equal(unescape(escapedCode(quoted)), '$x$\n[[Note]]');
  const listed = parser.render('- item\n\n  ```text\n  $x$\n  <!-- -->\n  ```');
  assert.equal(unescape(escapedCode(listed)), '$x$\n<!-- -->');
});

test('unclosed fences keep their payload literal', () => {
  const html = parser.render('~~~text\n$x$\n<!-- -->');
  assert.equal(unescape(escapedCode(html)), '$x$\n<!-- -->');
  assert.deepEqual(mathPayloads(html), []);
});

test('fence rendering preserves leading, internal and extra trailing blank lines', () => {
  const code = '\nfirst\n\n\nlast\n\n';
  const html = parser.render('```text\n' + code + '\n```');
  assert.equal(unescape(escapedCode(html)), code);
});

test('inline and multiline code protect math, wiki and legacy links', () => {
  const html = parser.render('`` $x$ [[Note]] [a](a b) ` `` and `line\n$x$`', { wikiLinkTitles: [] });
  assert.match(html, /<code[^>]*>\$x\$ \[\[Note\]\] \[a\]\(a b\) `<\/code>/);
  assert.match(html, /<code[^>]*>line \$x\$<\/code>/);
  assert.deepEqual(mathPayloads(html), []);
});

test('escaped dollars stay prose while real math retains exact escaped-dollar and backtick TeX', () => {
  const html = parser.render('Escaped \\$x$ and $a\\$b$ and $`raw`$; $10 to $50 and $ x$ stay prose.');
  assert.deepEqual(mathPayloads(html), ['a\\$b', '`raw`']);
  assert.match(html, /Escaped \$x\$/);
  assert.match(html, /\$10 to \$50/);
});

test('math payloads are consumed before wiki, link and task transformations', () => {
  const tex = '[[Note]]\n[label](path with spaces)\n```\n- [x] TeX';
  const html = parser.render('$$\n' + tex + '\n$$\n\n$x+y$', { wikiLinkTitles: [] });
  assert.deepEqual(mathPayloads(html), [tex, 'x+y']);
  assert.doesNotMatch(html, /data-wiki-link|data-checked|<pre>/);
});

test('unclosed display math stays literal and normal fenced indentation convention stays unchanged', () => {
  assert.deepEqual(mathPayloads(parser.render('$$\nunclosed')), []);
  assert.doesNotMatch(parser.render('\tordinary tab-indented text'), /<pre>/);
});

test('semantic math ranges do not mistake fenced or inline code for display math', () => {
  const source = '~~~text\n$$\n# code\n$$\n~~~\n\n> $$\n> x\n> ---\n> $$\n\n# Real';
  const ranges = displayMathRanges(source);
  assert.equal(ranges.length, 1);
  assert.equal(source.slice(ranges[0].from, ranges[0].to), '> $$\n> x\n> ---\n> $$\n');
  assert.deepEqual(displayMathRanges('```text\n$$\n```\n\n# Real'), []);
});

test('wiki lookup preserves path precedence, shallowest title and escaped attributes', () => {
  const env = { wikiLinkTitles: [
    { path: 'deep/nested/Note.md', title: 'Note' }, { path: 'folder/Note.md', title: 'NOTE' },
    { path: 'other/Note.md', title: 'Note' }, { path: 'Quoted".md', title: 'Quoted"' },
  ] };
  const html = parser.render('[[Note]] [[deep/nested/Note#Heading|Alias]] [[Quoted"]] [[Missing]]', env);
  assert.match(html, /data-path="folder\/Note\.md"/);
  assert.match(html, /data-path="deep\/nested\/Note\.md" data-title="deep\/nested\/Note#Heading"[^>]*>Alias<\/span>/);
  assert.match(html, /data-path="Quoted&quot;\.md" data-title="Quoted&quot;"/);
  assert.match(html, /data-path="" data-title="Missing"/);
  assert.doesNotMatch(parser.render('\\[[Note]]', env), /data-wiki-link/);
  assert.doesNotMatch(parser.render('[[Note]]'), /data-wiki-link/);
});

test('wiki lookup reads titles once per render and refreshes after in-place cache changes', () => {
  let reads = 0;
  const notes = Array.from({ length: 250 }, (_, index) => ({
    path: `folder/Note ${index}.md`, get title() { reads++; return `Note ${index}`; },
  }));
  const env = { wikiLinkTitles: notes };
  const source = '[[Note 249]] '.repeat(100);
  parser.render(source, env);
  assert.equal(reads, notes.length);
  notes[249] = { path: 'Moved.md', title: 'Note 249' };
  assert.match(parser.render(source, env), /data-path="Moved\.md"/);
});

test('ordinary and legacy links/images remain functional while unsafe schemes remain rejected', () => {
  const html = parser.render('[a](<path with spaces> "title") [b](path with spaces) [c](path%20"title") ![d](<image path>%20"title") ![e](///home/user/image.png)');
  assert.match(html, /href="path%20with%20spaces"/);
  assert.match(html, /href="path"/);
  assert.match(html, /src="image%20path"/);
  assert.match(html, /src="\/home\/user\/image\.png"/);
  assert.doesNotMatch(html, / title=/);
  assert.doesNotMatch(parser.render('[bad](javascript:alert with spaces) ![bad](data:text/html;base64,foo bar)'), /href=|src=/);
});

test('compatibility destinations retain quoted parentheses and native nested URLs', () => {
  const html = parser.render('[a](path with spaces "a (title)") [b](<image path> (title)) [c](path(one))');
  assert.match(html, /href="path%20with%20spaces"/);
  assert.match(html, /href="image%20path"/);
  assert.match(html, /href="path\(one\)"/);
});

test('task and empty-paragraph handling uses real block structure, including nested and empty tasks', () => {
  const html = parser.render('- [x] **done**\n- [ ]\n  - [ ] nested\n\n<!-- -->\n# Next');
  assert.equal([...html.matchAll(/data-type="taskList"/g)].length, 2);
  assert.equal([...html.matchAll(/data-type="taskItem"/g)].length, 3);
  assert.match(html, /data-checked="true"[^>]*>[\s\S]*<strong>done<\/strong>/);
  assert.match(html, /<p><\/p>\n<h1>Next<\/h1>/);
  assert.doesNotMatch(parser.render('`- [x] not a task` and `<!-- -->`'), /data-checked|<p><\/p>/);
});

test('image-only gaps remain outside code and do not alter inline images', () => {
  assert.match(parser.render('Text\n\n![a](image.png)'), /<p>Text<\/p>\n<p><\/p>\n<p><img/);
  assert.doesNotMatch(parser.render('Text ![a](image.png)'), /<p><\/p>/);
});

test('only actual closed secret fences become secret blocks, not literal examples', () => {
  assert.match(parser.render('```helix-secret\nexample-payload\n```'), /data-secret-block="example-payload"/);
  const literal = '```helix-secret\nexample-payload\n```';
  assert.equal(unescape(escapedCode(parser.render('````text\n' + literal + '\n````'))), literal);
  assert.doesNotMatch(parser.render('```helix-secret\nunclosed'), /data-secret-block/);
});

test('invalid final secret-fence markers remain literal code', () => {
  for (const closing of ['    ```', '\t```', '```\u00a0']) {
    const html = parser.render('```helix-secret\nexample-payload\n' + closing);
    assert.doesNotMatch(html, /data-secret-block/);
    assert.equal(unescape(escapedCode(html)), 'example-payload\n' + closing);
  }
});

test('code whitespace is marked for DOM import without changing soft or hard line-break semantics', () => {
  const html = parser.render('# `   `\n\nbefore `trailing `\nnext  \nlast');
  assert.match(html, /<h1 data-helix-code="">/);
  assert.match(html, /<p data-helix-code="">/);
  assert.match(html, /<\/code> next<br>last/);
});
