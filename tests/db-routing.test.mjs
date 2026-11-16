import assert from 'node:assert/strict';
import test from 'node:test';
import { isPathInDirectory, isTrashedNote, resolveNoteReference, resolveFileReference, isNoteLink } from '../src/lib/utils/paths.ts';
test('database note identities remain owned identities across session restore and links', () => {
  const vault='/notes/fixture.rn', id='a9a8f14a-e276-4cc7-b706-51ff957ed46d';
  assert.equal(isPathInDirectory(id,vault),true);
  assert.equal(isPathInDirectory('/outside/file.md',vault),false);
  assert.equal(resolveNoteReference(id,vault),id);
  assert.equal(resolveFileReference('./'+id,id,vault).path,id);
  assert.equal(isNoteLink(id,vault),true);
  assert.equal(isNoteLink('rnattachment:///fixture/id',vault),false);
  assert.equal(isTrashedNote(id,vault,true),true);
  assert.equal(isTrashedNote(id,vault,false),false);
});
