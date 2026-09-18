import assert from 'node:assert/strict';
import test from 'node:test';
import {
  classifyClipboardType,
  collectClipboardPayload,
  isEmptyPayload,
  pastedFileName,
  plainTextFromUriList
} from './clipboard-paste.ts';

const item = entries => ({
  types: Object.keys(entries),
  getType: async type => {
    const value = entries[type];
    return value instanceof Blob ? value : new Blob([value], { type });
  }
});
const png = new Blob([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], { type: 'image/png' });

test('classifies text formats as text, attachments as files and metadata as ignored', () => {
  assert.equal(classifyClipboardType('text/plain'), 'plain');
  assert.equal(classifyClipboardType('text/plain;charset=utf-8'), 'plain');
  assert.equal(classifyClipboardType('TEXT/HTML'), 'html');
  for (const type of ['text/uri-list', 'text/rtf', 'text/x-moz-url', 'text/csv']) {
    assert.equal(classifyClipboardType(type), 'string', type);
  }
  for (const type of ['image/png', 'image/jpeg', 'image/svg+xml', 'application/pdf']) {
    assert.equal(classifyClipboardType(type), 'file', type);
  }
  for (const type of ['web application/x-custom', 'chromium/x-web-custom-data', 'application/x-moz-nativeimage', 'application/octet-stream', '']) {
    assert.equal(classifyClipboardType(type), 'ignore', type);
  }
});

test('a copied link stays text and never becomes a uri-list attachment', async () => {
  const payload = await collectClipboardPayload([item({ 'text/uri-list': 'https://example.com/a\r\n' })]);
  assert.equal(payload.files.length, 0);
  assert.equal(payload.text, 'https://example.com/a');
  assert.deepEqual(payload.strings, [['text/uri-list', 'https://example.com/a\r\n']]);
});

test('text/plain wins over the uri-list fallback', async () => {
  const payload = await collectClipboardPayload([item({ 'text/plain': 'Example', 'text/uri-list': 'https://example.com' })]);
  assert.equal(payload.text, 'Example');
  assert.equal(payload.files.length, 0);
});

test('uri-list comments and blank lines are skipped', () => {
  assert.equal(plainTextFromUriList('# copied from browser\n\n  https://a.example  \nhttps://b.example'), 'https://a.example');
  assert.equal(plainTextFromUriList('# only a comment\n'), '');
});

test('images and PDFs become named files alongside the text and HTML', async () => {
  const pdf = new Blob(['%PDF-1.7'], { type: 'application/pdf' });
  const payload = await collectClipboardPayload([
    item({ 'text/html': '<img src="https://example.com/cat.png">', 'image/png': png }),
    item({ 'application/pdf': pdf })
  ]);
  assert.equal(payload.html, '<img src="https://example.com/cat.png">');
  assert.deepEqual(payload.files.map(file => [file.name, file.type]), [
    ['pasted-image.png', 'image/png'],
    ['pasted-file-2.pdf', 'application/pdf']
  ]);
  assert.equal(isEmptyPayload(payload), false);
});

test('metadata-only clipboards are empty and are never read', async () => {
  const read = [];
  const payload = await collectClipboardPayload([{
    types: ['chromium/x-web-custom-data', 'web application/x-custom'],
    getType: async type => { read.push(type); return new Blob(['x']); }
  }]);
  assert.deepEqual(read, []);
  assert.equal(isEmptyPayload(payload), true);
});

test('pasted file names use friendly extensions', () => {
  assert.equal(pastedFileName('image/jpeg', 0), 'pasted-image.jpg');
  assert.equal(pastedFileName('image/svg+xml', 1), 'pasted-image-2.svg');
  assert.equal(pastedFileName('application/pdf', 0), 'pasted-file.pdf');
});
