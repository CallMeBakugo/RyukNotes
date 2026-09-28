import assert from 'node:assert/strict';
import test from 'node:test';
import { AppearanceMutations } from './appearance-mutations.ts';

const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

test('appearance reads started before or during a write cannot restore an old snapshot', async () => {
  const changes = new AppearanceMutations();
  const before = changes.snapshot('A');
  const gate = deferred();
  const write = changes.run('A', 'notebook', () => true, () => gate.promise, () => {});
  const during = changes.snapshot('A');
  assert.equal(changes.acceptsSnapshot('A', before), false);
  assert.equal(changes.acceptsSnapshot('A', during), false);
  assert.equal(changes.acceptsSnapshot('B', changes.snapshot('B')), true);
  gate.resolve();
  await write;
  assert.equal(changes.acceptsSnapshot('A', before), false);
  assert.equal(changes.acceptsSnapshot('A', during), false);
  assert.equal(changes.acceptsSnapshot('A', changes.snapshot('A')), true);
});

test('an upload and later field patch commit in order without losing either field', async () => {
  const changes = new AppearanceMutations();
  const gate = deferred();
  let persisted = { color: '#abcdef' };
  let visible;
  const upload = changes.run('A', 'work', () => true, async () => {
    const original = persisted;
    await gate.promise;
    persisted = { ...original, icon: 'uploaded.svg' };
    return persisted;
  }, next => { visible = next; });
  const color = changes.run('A', 'work', () => true, async () => {
    persisted = { ...persisted, color: '#123456' };
    return persisted;
  }, next => { visible = next; });
  gate.resolve();
  await Promise.all([upload, color]);
  assert.deepEqual(persisted, { icon: 'uploaded.svg', color: '#123456' });
  assert.deepEqual(visible, persisted);
});

test('a superseded queued choice cannot write over the newer choice', async () => {
  const changes = new AppearanceMutations();
  const gate = deferred();
  const blocker = changes.run('A', 'work', () => true, () => gate.promise, () => {});
  const oldChoice = changes.choose('A', 'work:icon');
  let persisted = 'original';
  const old = changes.run('A', 'work', oldChoice, async () => { persisted = 'old'; }, () => {});
  const latest = changes.choose('A', 'work:icon');
  const next = changes.run('A', 'work', latest, async () => { persisted = 'new'; }, () => {});
  gate.resolve();
  await Promise.all([blocker, old, next]);
  assert.equal(persisted, 'new');
  assert.equal(oldChoice(), false);
  assert.equal(latest(), true);
});

test('a failed later choice does not hide the successful earlier commit or poison the queue', async () => {
  const changes = new AppearanceMutations();
  let visible = 'original';
  const first = changes.run('A', 'work', () => true, async () => 'uploaded.svg', value => { visible = value; });
  const failed = changes.run('A', 'work', () => true, async () => { throw new Error('disk full'); }, value => { visible = value; });
  await first;
  await assert.rejects(failed, /disk full/);
  assert.equal(visible, 'uploaded.svg');
  await changes.run('A', 'work', () => true, async () => 'builtin:star', value => { visible = value; });
  assert.equal(visible, 'builtin:star');
  assert.equal(changes.acceptsSnapshot('A', changes.snapshot('A')), true);
});

test('different vaults do not share choice cancellation or a write queue', async () => {
  const changes = new AppearanceMutations();
  const aChoice = changes.choose('A', 'work');
  changes.choose('B', 'work');
  const gate = deferred();
  const a = changes.run('A', 'work', aChoice, () => gate.promise, () => {});
  let bSaved = false;
  await changes.run('B', 'work', () => true, async () => { bSaved = true; }, () => {});
  assert.equal(aChoice(), true);
  assert.equal(bSaved, true);
  gate.resolve();
  await a;
});
