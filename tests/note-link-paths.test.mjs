import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { transformWithEsbuild } from 'vite';

const source = await readFile(
  new URL('../src/lib/utils/paths.ts', import.meta.url),
  'utf8'
);
const { code } = await transformWithEsbuild(source, 'paths.ts', {
  loader: 'ts',
  format: 'esm',
  target: 'esnext'
});
const {
  assetSourceToMarkdown,
  assetUrlToLocalPath,
  normalizeLocalAssetPath,
  isPathInDirectory,
  isTrashedNote,
  resolvePathFromFile,
  resolveVaultFilePath,
  resolveFileReference,
  noteAssetRoot
} = await import(
  `data:text/javascript;base64,${Buffer.from(code).toString('base64')}`
);

test('resolves note links relative to Windows note paths', () => {
  assert.equal(
    resolvePathFromFile('C:\\Vault\\Current.md', './Target.md'),
    'C:/Vault/Target.md'
  );
  assert.equal(
    resolvePathFromFile('C:\\Vault\\Folder\\Current.md', '../Target.md'),
    'C:/Vault/Target.md'
  );
  assert.equal(
    resolvePathFromFile('\\\\server\\share\\Folder\\Current.md', '../Target.md'),
    '//server/share/Target.md'
  );
});

test('preserves Linux relative note-link resolution', () => {
  assert.equal(
    resolvePathFromFile('/vault/folder/Current.md', '../Target.md'),
    '/vault/Target.md'
  );
});

test('converts Windows asset URLs back to portable attachment paths', () => {
  assert.equal(
    normalizeLocalAssetPath('/C:\\Users\\user\\Vault\\image.png'),
    'C:/Users/user/Vault/image.png'
  );
  assert.equal(
    assetSourceToMarkdown(
      'http://asset.localhost/C%3A%5CUsers%5Cuser%5CVault%5C.ryuknotes%5Cattachments%5Cimage.png',
      'C:\\Users\\user\\Vault\\Note.md',
      'C:\\Users\\user\\Vault'
    ),
    '.ryuknotes/attachments/image.png'
  );
});

test('preserves portable asset paths on Linux and macOS', () => {
  assert.equal(
    assetSourceToMarkdown(
      'asset://localhost/%2Fhome%2Fuser%2FVault%2F.ryuknotes%2Fattachments%2Fimage.png',
      '/home/user/Vault/Note.md',
      '/home/user/Vault'
    ),
    '.ryuknotes/attachments/image.png'
  );
  assert.equal(
    assetSourceToMarkdown(
      'asset://localhost/%2FUsers%2Fuser%2FVault%2Fassets%2Fimage.png',
      '/Users/user/Vault/notes/Note.md',
      '/Users/user/Vault'
    ),
    '../assets/image.png'
  );
  assert.equal(
    assetSourceToMarkdown('../assets/image.png', '/vault/notes/Note.md', '/vault'),
    '../assets/image.png'
  );
});

test('decodes local asset URLs without damaging platform roots', () => {
  assert.equal(
    assetUrlToLocalPath('http://asset.localhost/C%3A%5CUsers%5Cuser%5CVault%5Cimage.png'),
    'C:/Users/user/Vault/image.png'
  );
  assert.equal(
    assetUrlToLocalPath('asset://localhost/%2Fhome%2Fuser%2FVault%2Fimage.png'),
    '/home/user/Vault/image.png'
  );
  assert.equal(
    assetUrlToLocalPath('http://asset.localhost/%5C%5Cserver%5Cshare%5CVault%5Cimage.png'),
    '//server/share/Vault/image.png'
  );
});

test('resolves vault files consistently across platforms', () => {
  assert.equal(
    resolveVaultFilePath('../assets/image.png', 'C:\\Vault\\notes\\Note.md', 'C:\\Vault'),
    'C:/Vault/assets/image.png'
  );
  assert.equal(
    resolveVaultFilePath('.ryuknotes/attachments/image.png', 'C:\\Vault\\notes\\Note.md', 'C:\\Vault'),
    'C:/Vault/.ryuknotes/attachments/image.png'
  );
  assert.equal(
    resolveVaultFilePath('../assets/image.png', '/vault/notes/Note.md', '/vault'),
    '/vault/assets/image.png'
  );
  assert.equal(
    resolveVaultFilePath('../assets/image.png', '\\\\server\\share\\Vault\\notes\\Note.md', '\\\\server\\share\\Vault'),
    '//server/share/Vault/assets/image.png'
  );
});

test('document URI suffixes are excluded from filesystem paths without double-decoding filename escapes', () => {
  assert.deepEqual(resolveFileReference('.ryuknotes/attachments/report%2520%29.pdf#page=2', '/vault/Notes/Note.md', '/vault'), {
    path: '/vault/.ryuknotes/attachments/report%20).pdf', suffix: '#page=2'
  });
  assert.deepEqual(resolveFileReference('../files/report%23%3F.pdf?mode=fit#page=2', '/vault/Notes/Note.md', '/vault'), {
    path: '/vault/files/report#?.pdf', suffix: '?mode=fit#page=2'
  });
});

test('older imported managed links stay inside their vault after moving to another notebook depth', () => {
  for (const note of ['/vault/Projects/Nested/Note.md', '/vault/Note.md', '/vault/Archive/Note.md']) {
    assert.equal(resolveFileReference('../../.ryuknotes/attachments/report.pdf#page=2', note, '/vault').path, '/vault/.ryuknotes/attachments/report.pdf');
  }
});

test('external source roots use configured ownership without borrowing the active vault', () => {
  assert.equal(noteAssetRoot('/source/Notes/Note.md', '/destination', ['/source']), '/source');
  assert.equal(noteAssetRoot('/external/Note.md', '/destination', ['/source']), null);
  assert.equal(noteAssetRoot('/destination-extra/Note.md', '/destination'), null);
  assert.equal(noteAssetRoot('C:\\SOURCE\\Note.md', 'C:\\Destination', ['c:/Source']), 'c:/Source');
});

test('restored Windows and UNC notes remain vault-owned across separator and case differences', () => {
  assert.equal(isPathInDirectory('C:\\NOTES\\Alpha.md', 'c:\\Notes\\'), true);
  assert.equal(isPathInDirectory('C:/Notes/Folder/Alpha.md', 'C:\\Notes'), true);
  assert.equal(isPathInDirectory('\\\\SERVER\\SHARE\\Notes\\Alpha.md', '//server/share/notes/'), true);
});

test('vault ownership normalizes trailing separators and dot segments without accepting siblings', () => {
  assert.equal(isPathInDirectory('/vault/folder/../Alpha.md', '/vault/'), true);
  assert.equal(isPathInDirectory('/vault/folder/../../Outside.md', '/vault'), false);
  assert.equal(isPathInDirectory('/vault-extra/Alpha.md', '/vault'), false);
  assert.equal(isPathInDirectory('D:\\Notes\\Alpha.md', 'C:\\Notes'), false);
  assert.equal(isPathInDirectory('/Vault/Alpha.md', '/vault'), false);
  assert.equal(isPathInDirectory('/vault/Alpha.md', null), false);
});

test('trash previews are protected without blocking ordinary notebooks or other vaults', () => {
  assert.equal(isTrashedNote('/vault/.ryuknotes/trash/deleted.md', '/vault'), true);
  assert.equal(isTrashedNote('C:\\Vault\\.ryuknotes\\trash\\deleted.md', 'c:/vault/'), true);
  assert.equal(isTrashedNote('/vault/Trash/note.md', '/vault'), false);
  assert.equal(isTrashedNote('/vault/.ryuknotes/trash-backup/note.md', '/vault'), false);
  assert.equal(isTrashedNote('/other/.ryuknotes/trash/deleted.md', '/vault'), false);
  assert.equal(isTrashedNote(null, '/vault'), false);
});
