import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createContext, Script } from 'node:vm';
import { parse } from 'svelte/compiler';
import { transformWithEsbuild } from 'vite';
import * as paths from '../src/lib/utils/paths.ts';
import { createEditorMarkdownParser } from '../src/lib/editor/markdownParser.ts';

const source = await readFile(new URL('../src/lib/components/Editor.svelte', import.meta.url), 'utf8');
const declaration = parse(source, { modern: true }).instance.content.body.find(node =>
  node.type === 'FunctionDeclaration' && node.id.name === 'resolveImageSrc'
);
const owningRoot = parse(source, { modern: true }).instance.content.body.find(node => node.type === 'FunctionDeclaration' && node.id.name === 'currentNoteAssetRoot');
const { code } = await transformWithEsbuild(`${owningRoot ? source.slice(owningRoot.start, owningRoot.end) : ''}\n${source.slice(declaration.start, declaration.end)}`, 'resolve-image.ts', { loader: 'ts', format: 'esm' });
const script = new Script(`${code}\nresolveImageSrc;`);
const parser = createEditorMarkdownParser();
const imageSource = markdown => parser.parseInline(markdown, {}).flatMap(token => token.children ?? []).find(token => token.type === 'image')?.attrGet('src');
const assetPath = url => decodeURIComponent(new URL(url).pathname.slice(1));
const encodedName = name => encodeURIComponent(name).replace(/[!'()*]/g, char => `%${char.charCodeAt(0).toString(16).toUpperCase()}`);
function fixture(note = '/vault/Projects/Note.md', vault = '/vault', known = []) {
  const context = createContext({ ...paths,
    $activeNotePath: note, $appConfig: { active_vault: vault, vaults: known.map(path => ({ path })) },
    normalizePath: path => paths.resolvePathFromFile('', path),
    convertFileSrc: path => `http://asset.localhost/${encodeURIComponent(path)}`,
  });
  return { context, resolve: script.runInContext(context) };
}

for (const name of ['photo%20one.png', 'photo).png', 'photo(one.png', 'photo#one?.png', "café 雪 + 'image'.png"]) {
  test(`rich save/reload preserves the actual attachment filename ${JSON.stringify(name)}`, () => {
    const f = fixture();
    const expected = `/vault/.ryuknotes/attachments/${name}`;
    let markdown = `![Photo](../.ryuknotes/attachments/${encodedName(name)}?v=1#preview)`;
    for (let round = 0; round < 3; round++) {
      const src = imageSource(markdown);
      assert.ok(src, 'round trips must not turn an image into malformed Markdown');
      const asset = f.resolve(src);
      assert.equal(assetPath(asset), expected, 'query and fragment never become part of the filesystem path');
      assert.equal(new URL(asset).search, '?v=1');
      assert.equal(new URL(asset).hash, '#preview');
      markdown = `![Photo](${paths.assetSourceToMarkdown(asset, f.context.$activeNotePath, '/vault')})`;
    }
  });
}

test('Windows and UNC asset URLs preserve encoded filenames across repeated saves', () => {
  for (const [vault, note, absolute] of [
    ['C:\\Notes', 'C:\\Notes\\Projects\\Note.md', 'C:/Notes/.ryuknotes/attachments/photo%20).png'],
    ['\\\\server\\share\\Notes', '\\\\server\\share\\Notes\\Projects\\Note.md', '//server/share/Notes/.ryuknotes/attachments/photo%20).png']
  ]) {
    const f = fixture(note, vault);
    let markdown = '![Photo](.ryuknotes/attachments/photo%2520%29.png#preview)';
    for (let round = 0; round < 3; round++) {
      const asset = f.resolve(imageSource(markdown));
      assert.equal(assetPath(asset), absolute);
      markdown = `![Photo](${paths.assetSourceToMarkdown(asset, note, vault)})`;
    }
  }
});

test('an external note uses its configured source vault, not the active destination vault', () => {
  const f = fixture('/source/Notebook/Note.md', '/destination', ['/source', '/destination']);
  assert.equal(assetPath(f.resolve('.ryuknotes/attachments/image.png')), '/source/.ryuknotes/attachments/image.png');
  assert.equal(assetPath(f.resolve('../.ryuknotes/attachments/image.png')), '/source/.ryuknotes/attachments/image.png');
});

test('an unconfigured external note resolves its own relative assets without borrowing active-vault metadata', () => {
  const f = fixture('/external/Note.md', '/destination');
  assert.equal(assetPath(f.resolve('.ryuknotes/attachments/image.png')), '/external/.ryuknotes/attachments/image.png');
  assert.equal(assetPath(f.resolve('images/image.png')), '/external/images/image.png');
});

for (const name of ['Screenshot #1.png', '100% (final).png', 'a%20b #c?d.png', "café 雪 + 'x'.png"]) {
  test(`a newly stored attachment ${JSON.stringify(name)} resolves to its own file and saves back unchanged`, () => {
    const f = fixture();
    // The backend returns the raw stored path, which uploadAsset must encode before using it as a destination.
    const stored = `.ryuknotes/attachments/0b1c2d3e_${name}`;
    const destination = paths.encodeMarkdownPath(stored);
    assert.equal(imageSource(`![x](${destination})`), destination, 'the encoded destination parses as one image');
    const asset = f.resolve(destination);
    assert.equal(assetPath(asset), `/vault/${stored}`);
    assert.equal(new URL(asset).search, '');
    assert.equal(paths.assetSourceToMarkdown(asset, '/vault/Projects/Note.md', '/vault'), destination);
  });
}

test('uploadAsset encodes the stored path before every Markdown or URI use', () => {
  const start = source.indexOf('async function uploadAsset');
  const body = source.slice(start, source.indexOf('async function insertFile'));
  assert.match(body, /const destination = encodeMarkdownPath\(relativePath\)/);
  assert.equal(body.replace('encodeMarkdownPath(relativePath)', '').includes('relativePath)'), false);
  assert.equal((body.match(/\$\{relativePath\}|src: relativePath|href: relativePath|resolveImageSrc\(relativePath\)/g) ?? []).length, 0);
});

test('Windows drive letters stay literal when a path outside the vault is written to Markdown', () => {
  assert.equal(paths.encodeMarkdownPath('C:/Users/me/pic one.png'), 'C:/Users/me/pic%20one.png');
  assert.equal(paths.encodeMarkdownPath('C:\\Users\\me\\a#b.png'), 'C:/Users/me/a%23b.png');
  assert.equal(paths.encodeMarkdownPath('/home/me/pic one.png'), '/home/me/pic%20one.png');
  assert.equal(paths.encodeMarkdownPath('relative/a:b.png'), 'relative/a%3Ab.png');
  assert.equal(
    paths.assetSourceToMarkdown('http://asset.localhost/C%3A%2FUsers%2Fme%2Fpic%20one.png', '/v/Note.md', '/v'),
    'C:/Users/me/pic%20one.png',
  );
  assert.equal(paths.resolveFileReference('C:/Users/me/pic%20one.png', '/v/Note.md', '/v').path, 'C:/Users/me/pic one.png');
});
