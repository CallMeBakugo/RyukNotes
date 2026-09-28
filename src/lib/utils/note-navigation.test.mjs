import assert from 'node:assert/strict';
import test from 'node:test';
import { beginNoteNavigation, isCurrentNoteNavigation, cancelNoteNavigation } from './note-navigation.ts';

function deferred() {
  let resolve;
  const promise = new Promise(res => { resolve = res; });
  return { promise, resolve };
}

function navigationConsumer(initialSelection) {
  let selected = initialSelection;
  let ownedToken;
  return {
    get selected() { return selected; },
    async select(path, read) {
      const token = beginNoteNavigation();
      ownedToken = token;
      if (path === selected.path) return;
      const note = await read;
      if (isCurrentNoteNavigation(token)) selected = note;
    },
    dispose() {
      if (ownedToken !== undefined) cancelNoteNavigation(ownedToken);
    },
  };
}

test('a slower earlier note read cannot replace a newer selection', async () => {
  const navigation = navigationConsumer({ path: 'initial.md', body: 'initial' });
  const older = deferred();
  const newer = deferred();
  const oldSelection = navigation.select('older.md', older.promise);
  const newSelection = navigation.select('newer.md', newer.promise);
  newer.resolve({ path: 'newer.md', body: 'newer note' });
  await newSelection;
  older.resolve({ path: 'older.md', body: 'older note' });
  await oldSelection;
  assert.deepEqual(navigation.selected, { path: 'newer.md', body: 'newer note' });
});

test('selecting the already-open note cancels a different pending selection', async () => {
  const currentNote = { path: 'current.md', body: 'current note' };
  const navigation = navigationConsumer(currentNote);
  const pending = deferred();
  const selection = navigation.select('other.md', pending.promise);
  await navigation.select('current.md');
  pending.resolve({ path: 'other.md', body: 'other note' });
  await selection;
  assert.equal(navigation.selected, currentNote);
});

test('disposing the owner prevents its unfinished read from publishing', async () => {
  const currentNote = { path: 'current.md', body: 'current note' };
  const navigation = navigationConsumer(currentNote);
  const pending = deferred();
  const selection = navigation.select('other.md', pending.promise);
  navigation.dispose();
  pending.resolve({ path: 'other.md', body: 'old vault content' });
  await selection;
  assert.equal(navigation.selected, currentNote);
});

test('disposing an older owner does not cancel a replacement owner selection', async () => {
  const oldNote = { path: 'old-vault/current.md', body: 'old vault' };
  const oldOwner = navigationConsumer(oldNote);
  const newOwner = navigationConsumer({ path: 'new-vault/current.md', body: 'new vault' });
  const oldRead = deferred();
  const newRead = deferred();
  const oldSelection = oldOwner.select('old-vault/other.md', oldRead.promise);
  const newSelection = newOwner.select('new-vault/other.md', newRead.promise);
  oldOwner.dispose();
  oldRead.resolve({ path: 'old-vault/other.md', body: 'stale old vault' });
  newRead.resolve({ path: 'new-vault/other.md', body: 'latest new vault' });
  await Promise.all([oldSelection, newSelection]);
  assert.equal(oldOwner.selected, oldNote);
  assert.deepEqual(newOwner.selected, { path: 'new-vault/other.md', body: 'latest new vault' });
});
