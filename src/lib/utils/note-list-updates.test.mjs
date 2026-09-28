import assert from 'node:assert/strict';
import test from 'node:test';
import { isCurrentVaultEvent, updateNoteList, sameNoteTags } from './note-list-updates.ts';

const note = {
  path: '/vault/Work/Plan.md', relative_path: 'Work/Plan.md', preview: 'Before',
  meta: { id: 'plan', title: 'Plan', tags: ['work'], pinned: false, created: '2026-01-01', modified: '2026-01-02' },
};

test('a body save updates preview and modification date without changing unrelated notes', () => {
  const other = { ...note, path: '/vault/Other.md', relative_path: 'Other.md' };
  const changed = { ...note, preview: 'After', meta: { ...note.meta, modified: '2026-01-03' } };
  const result = updateNoteList([note, other], changed, 'all', []);
  assert.deepEqual(result, [changed, other]);
  assert.equal(result[1], other);
  assert.equal(sameNoteTags(note, changed), true);
});

test('tag edits remove and insert notes in the appropriate cached views', () => {
  const changed = { ...note, meta: { ...note.meta, tags: ['personal'] } };
  assert.deepEqual(updateNoteList([note], changed, 'tag:work', []), []);
  assert.deepEqual(updateNoteList([], changed, 'tag:personal', []), [changed]);
  assert.equal(sameNoteTags(note, changed), false);
});

test('notebook views contain direct children only, with native path separators', () => {
  const windows = { ...note, path: 'C:\\vault\\Work\\Plan.md', relative_path: 'Work\\Plan.md' };
  assert.deepEqual(updateNoteList([], windows, 'nb:C:/vault/Work', []), [windows]);
  assert.deepEqual(updateNoteList([], windows, 'nb:C:/vault', []), []);
  assert.deepEqual(updateNoteList([], windows, 'nb:C:/vault/Workshop', []), []);
});

test('daily, quick access and trash views keep their membership constraints', () => {
  const daily = { ...note, path: '/vault/Daily/2026-01-03.md', relative_path: 'Daily/2026-01-03.md' };
  assert.deepEqual(updateNoteList([], daily, 'daily', []), [daily]);
  assert.deepEqual(updateNoteList([], note, 'daily', []), []);
  assert.deepEqual(updateNoteList([], note, 'quickaccess', ['Work\\Plan.md']), [note]);
  assert.deepEqual(updateNoteList([note], note, 'quickaccess', []), []);
  assert.deepEqual(updateNoteList([], note, 'trash', []), []);
});

test('duplicate watcher notifications leave an already current list unchanged', () => {
  const list = [note];
  assert.equal(updateNoteList(list, structuredClone(note), 'all', []), list);
});

test('delayed events cannot cross vaults, including nested vaults sharing note paths', () => {
  const event = { vault_path: '/vault', event_type: 'modify', path: '/vault/nested/Note.md', note };
  assert.equal(isCurrentVaultEvent(event, '/vault'), true);
  assert.equal(isCurrentVaultEvent(event, '/other'), false);
  assert.equal(isCurrentVaultEvent(event, '/vault/nested'), false);
  assert.equal(isCurrentVaultEvent({ ...event, vault_path: '/vault/nested' }, '/vault'), false);
  assert.equal(isCurrentVaultEvent(event, null), false);
  const unscoped = JSON.parse('{"event_type":"modify","path":"/vault/nested/Note.md"}');
  assert.equal(isCurrentVaultEvent(unscoped, '/vault'), false);
});

test('vault event identity handles native Windows separators and trailing slashes', () => {
  const event = { vault_path: 'C:\\Notes\\Vault\\', event_type: 'remove', path: 'C:\\Notes\\Vault\\Old.md' };
  assert.equal(isCurrentVaultEvent(event, 'C:/Notes/Vault'), true);
  assert.equal(isCurrentVaultEvent(event, 'C:/Notes/Vault-copy'), false);
});
