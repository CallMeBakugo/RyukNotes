import assert from 'node:assert/strict';
import test from 'node:test';
import { RevisionSaveQueue } from './revision-save.ts';

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

test('an edit during a pending save stays dirty and the next save persists it', async () => {
  const saves = new RevisionSaveQueue();
  const started = deferred();
  const diskWrite = deferred();
  let disk = 'initial';
  saves.markChanged();
  const first = saves.save(saves.capture(), async () => {
    started.resolve();
    await diskWrite.promise;
    disk = 'first edit';
  });
  await started.promise;
  saves.markChanged();
  diskWrite.resolve();
  assert.equal(await first, false, 'a stale completion must not authorize navigation');
  assert.equal(saves.dirty, true);
  assert.equal(await saves.save(saves.capture(), async () => { disk = 'second edit'; }), true);
  assert.equal(disk, 'second edit');
  assert.equal(saves.dirty, false);
});

test('failed saves stay dirty and do not poison an explicit retry', async () => {
  const saves = new RevisionSaveQueue();
  let disk = 'initial';
  saves.markChanged();
  const snapshot = saves.capture();
  await assert.rejects(saves.save(snapshot, async () => { throw new Error('disk full'); }), /disk full/);
  assert.equal(saves.dirty, true);
  assert.equal(disk, 'initial');
  assert.equal(await saves.save(snapshot, async () => { disk = 'retried edit'; }), true);
  assert.equal(disk, 'retried edit');
  assert.equal(saves.dirty, false);
});

test('flush waits behind autosave and an older captured snapshot cannot overwrite it', async () => {
  const saves = new RevisionSaveQueue();
  const started = deferred();
  const diskWrite = deferred();
  const writes = [];
  saves.markChanged();
  const older = saves.capture();
  const autoSave = saves.save(older, async () => {
    started.resolve();
    await diskWrite.promise;
    writes.push('autosave');
  });
  await started.promise;
  saves.markChanged();
  const flush = saves.save(saves.capture(), async () => { writes.push('flush'); });
  const staleFlush = saves.save(older, async () => { writes.push('stale'); });
  assert.deepEqual(writes, []);
  diskWrite.resolve();
  assert.equal(await autoSave, false);
  assert.equal(await flush, true);
  assert.equal(await staleFlush, false);
  assert.deepEqual(writes, ['autosave', 'flush']);
});

test('returning to the same note starts a new session that an older completion cannot clean', async () => {
  const saves = new RevisionSaveQueue();
  const started = deferred();
  const diskWrite = deferred();
  const writes = [];
  saves.markChanged();
  const leaving = saves.save(saves.capture(), async () => {
    started.resolve();
    await diskWrite.promise;
    writes.push(['A.md', 'before leaving']);
  });
  await started.promise;
  saves.startSession();
  saves.markChanged();
  const otherNote = saves.save(saves.capture(), async () => { writes.push(['B.md', 'other note']); });
  saves.startSession();
  saves.markChanged();
  diskWrite.resolve();
  assert.equal(await leaving, false);
  assert.equal(await otherNote, false);
  assert.equal(saves.dirty, true);
  assert.equal(await saves.save(saves.capture(), async () => { writes.push(['A.md', 'after returning']); }), true);
  assert.deepEqual(writes, [
    ['A.md', 'before leaving'],
    ['B.md', 'other note'],
    ['A.md', 'after returning'],
  ]);
  assert.equal(saves.dirty, false);
});

test('queued saves advance the exact accepted raw baseline even when newer typing remains dirty', async () => {
  const saves = new RevisionSaveQueue();
  const firstWrite = deferred();
  const started = deferred();
  let disk = '---\nmodified: old\n---\noriginal';
  saves.startSession(disk);
  saves.markChanged();
  const first = saves.save(saves.capture(), async (expectedRaw) => {
    assert.equal(expectedRaw, disk);
    started.resolve();
    await firstWrite.promise;
    disk = '---\nmodified: first\n---\nfirst edit';
    return disk;
  });
  await started.promise;
  saves.markChanged();
  firstWrite.resolve();
  assert.equal(await first, false);
  assert.equal(saves.dirty, true);
  assert.equal(saves.raw, disk, 'a successful older save advances the active session baseline');
  const second = saves.save(saves.capture(), async (expectedRaw) => {
    assert.equal(expectedRaw, disk, 'later queued saves compare against the preceding accepted raw');
    disk = '---\nmodified: second\n---\nsecond edit';
    return disk;
  });
  assert.equal(await second, true);
  assert.equal(saves.raw, disk);
  assert.equal(saves.dirty, false);
});

test('external disk changes reject an edit without losing its baseline or silently replacing disk', async () => {
  const saves = new RevisionSaveQueue();
  saves.startSession('original');
  saves.markChanged();
  let disk = 'external';
  const snapshot = saves.capture();
  const write = async (expectedRaw) => {
    if (expectedRaw !== disk) throw new Error('Save conflict: changed outside the app');
    disk = 'draft';
    return disk;
  };
  await assert.rejects(saves.save(snapshot, write), /Save conflict/);
  assert.equal(disk, 'external');
  assert.equal(saves.raw, 'original');
  assert.equal(saves.dirty, true);
  await assert.rejects(saves.save(snapshot, write), /Save conflict/);
  assert.equal(disk, 'external');
});

test('native rename advances a dirty draft baseline only for its accepted raw', async () => {
  const saves = new RevisionSaveQueue();
  saves.startSession('before rename');
  saves.markChanged();
  assert.equal(saves.adoptMovedRaw('stale unrelated revision', 'renamed'), false);
  assert.equal(saves.raw, 'before rename');
  assert.equal(saves.adoptMovedRaw('before rename', 'renamed'), true);
  assert.equal(saves.dirty, true, 'native path migration does not claim the draft was saved');
  let disk = 'renamed';
  assert.equal(await saves.save(saves.capture(), async (expectedRaw) => {
    assert.equal(expectedRaw, disk);
    disk = 'renamed with draft';
    return disk;
  }), true);
  assert.equal(saves.raw, disk);
  assert.equal(saves.dirty, false);
});

test('overlapping manual saves and a clean save do not rewrite the same revision', async () => {
  const saves = new RevisionSaveQueue();
  let writes = 0;
  const persist = async () => { writes++; };
  assert.equal(await saves.save(saves.capture(), persist), true);
  assert.equal(writes, 0);
  saves.markChanged();
  const snapshot = saves.capture();
  assert.deepEqual(await Promise.all([saves.save(snapshot, persist), saves.save(snapshot, persist)]), [true, true]);
  assert.equal(await saves.save(saves.capture(), persist), true);
  assert.equal(writes, 1);
});

test('autosave waits for idle but continuous typing cannot postpone it indefinitely', (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const saves = new RevisionSaveQueue();
  let now = 0;
  const writes = [];
  const save = () => { writes.push(now); };
  saves.schedule(save);
  for (let second = 1; second <= 9; second++) {
    now = second * 1_000;
    t.mock.timers.tick(1_000);
    saves.schedule(save);
  }
  assert.deepEqual(writes, []);
  now = 10_000;
  t.mock.timers.tick(1_000);
  assert.deepEqual(writes, [10_000]);
  saves.schedule(save);
  now = 12_999;
  t.mock.timers.tick(2_999);
  assert.deepEqual(writes, [10_000]);
  now = 13_000;
  t.mock.timers.tick(1);
  assert.deepEqual(writes, [10_000, 13_000]);
});

test('explicit saves and note switches cancel both pending autosave deadlines', (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const saves = new RevisionSaveQueue();
  const writes = [];
  saves.schedule(() => { writes.push('cancelled by flush'); });
  t.mock.timers.tick(1_000);
  saves.cancelScheduled();
  t.mock.timers.tick(10_000);
  saves.schedule(() => { writes.push('previous note'); });
  t.mock.timers.tick(1_000);
  saves.startSession();
  t.mock.timers.tick(10_000);
  saves.schedule(() => { writes.push('current note'); });
  t.mock.timers.tick(3_000);
  assert.deepEqual(writes, ['current note']);
});
