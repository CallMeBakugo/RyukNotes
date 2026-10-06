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
    const expected = `/vault/.helixnotes/attachments/${name}`;
    let markdown = `![Photo](../.helixnotes/attachments/${encodedName(name)}?v=1#preview)`;
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
    ['C:\\Notes', 'C:\\Notes\\Projects\\Note.md', 'C:/Notes/.helixnotes/attachments/photo%20).png'],
    ['\\\\server\\share\\Notes', '\\\\server\\share\\Notes\\Projects\\Note.md', '//server/share/Notes/.helixnotes/attachments/photo%20).png']
  ]) {
    const f = fixture(note, vault);
    let markdown = '![Photo](.helixnotes/attachments/photo%2520%29.png#preview)';
    for (let round = 0; round < 3; round++) {
      const asset = f.resolve(imageSource(markdown));
      assert.equal(assetPath(asset), absolute);
      markdown = `![Photo](${paths.assetSourceToMarkdown(asset, note, vault)})`;
    }
  }
});

test('an external note uses its configured source vault, not the active destination vault', () => {
  const f = fixture('/source/Notebook/Note.md', '/destination', ['/source', '/destination']);
  assert.equal(assetPath(f.resolve('.helixnotes/attachments/image.png')), '/source/.helixnotes/attachments/image.png');
  assert.equal(assetPath(f.resolve('../.helixnotes/attachments/image.png')), '/source/.helixnotes/attachments/image.png');
});

test('an unconfigured external note resolves its own relative assets without borrowing active-vault metadata', () => {
  const f = fixture('/external/Note.md', '/destination');
  assert.equal(assetPath(f.resolve('.helixnotes/attachments/image.png')), '/external/.helixnotes/attachments/image.png');
  assert.equal(assetPath(f.resolve('images/image.png')), '/external/images/image.png');
});
