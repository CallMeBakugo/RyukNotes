import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { runInNewContext } from 'node:vm';
import { randomUUID } from 'node:crypto';
import ts from 'typescript';

const source = await readFile(new URL('../src/lib/utils/note-recovery.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
function fixture() {
  let revision = 1;
  let body = 'draft one';
  let failedDisk = false;
  let failedForkReply = false;
  let forkGate;
  let registered = false;
  let pollChanges = false;
  let rereads = 0;
  const records = new Map();
  const forks = new Map();
  const calls = [];
  const notices = [];
  const handlers = new Map();
  let interval;
  const exports = {};
  const invoke = async (cmd, args) => {
    calls.push([cmd, args]);
    if (cmd === 'recovery_register') { registered = true; return; }
    assert.equal(registered, true);
    if (cmd === 'recovery_checkpoint') {
      if (failedDisk) throw new Error('disk full');
      records.set(args.draft.sequence, structuredClone(args.draft)); return;
    }
    if (cmd === 'recovery_resolve') {
      if (failedDisk) throw new Error('disk full');
      assert.ok(args.sequence === 0 || records.has(args.sequence)); return;
    }
    if (cmd === 'recovery_fork') {
      const record = records.get(args.sequence);
      assert.ok(record);
      if (forkGate) await forkGate.promise;
      if (!forks.has(args.sequence)) forks.set(args.sequence, { id: randomUUID(), content: record.body });
      if (failedForkReply) throw new Error('lost acknowledgment');
      return forks.get(args.sequence);
    }
    if (cmd === 'recovery_poll') return pollChanges;
  };
  const target = { addEventListener: (key, fn) => handlers.set(key, fn), removeEventListener: key => handlers.delete(key) };
  runInNewContext(js, { exports, require: name => name.includes('/event') ? { listen: async (event, fn) => { handlers.set(event, fn); return () => handlers.delete(event); } } : { invoke },
    crypto: { randomUUID }, structuredClone, setTimeout, clearTimeout, fetch: async () => { throw new Error('blob unavailable'); },
    window: target, document: { ...target, visibilityState: 'visible' },
    setInterval: fn => { interval = fn; return 1; }, clearInterval: () => {} });
  const base = { path: 'n', raw: 'base', content: 'base', meta: { id: 'n', title: 'Title', tags: [], pinned: false, created: '', modified: '' } };
  const owner = exports.createRecoveryOwner({ path: '/v.rn', file: 'inode' }, 'n', () => ({ revision, base, body, meta: base.meta }), message => notices.push(message));
  return { owner, records, forks, calls, notices, isConflict: exports.isRecoveryConflict, memory: exports.recoveryMemory, base,
    edit(value) { body = value; revision++; owner.edit(); },
    disk(value) { failedDisk = value; }, loseReply(value) { failedForkReply = value; },
    gate() { forkGate = deferred(); return forkGate; },
    async tick() { interval(); await new Promise(r => setTimeout(r, 0)); },
    async hint(event) { await handlers.get(event)?.(); await new Promise(r => setTimeout(r, 0)); },
    watch() { return owner.watch(async () => { rereads++; }); }, get rereads() { return rereads; },
  };
}

test('journal failure preserves the dirty buffer and refuses pause; a failed checkpoint is never durable', async () => {
  const f = fixture();
  f.disk(true);
  f.owner.edit();
  await assert.rejects(f.owner.checkpoint());
  assert.equal(f.owner.unresolved, true);
  assert.equal(await f.owner.pause(), false);
  assert.equal(f.memory.get(f.owner.owner).body, 'draft one');
  assert.equal(f.records.size, 0);
  assert.ok(f.notices.some(n => n.includes('in memory')));
  f.disk(false);
  const record = await f.owner.checkpoint();
  assert.equal(await f.owner.discard(record.revision), true);
  assert.equal(await f.owner.pause(), true);
});

test('fork retry reuses one operation; edits during fork cannot resolve a newer draft', async () => {
  const f = fixture();
  f.owner.conflict();
  await f.owner.checkpoint();
  f.loseReply(true);
  await assert.rejects(f.owner.fork());
  assert.equal(f.forks.size, 1);
  f.loseReply(false);
  const retry = await f.owner.fork();
  assert.equal(f.forks.size, 1);
  assert.equal(retry.note.content, 'draft one');
  const gate = f.gate();
  const inFlight = f.owner.fork();
  await new Promise(r => setTimeout(r, 0));
  f.edit('newer typing');
  gate.resolve();
  const result = await inFlight;
  assert.equal(await f.owner.discard(result.revision), false);
  assert.equal(f.owner.unresolved, true);
  assert.equal(f.memory.get(f.owner.owner).body, 'newer typing');
  assert.equal(await f.owner.pause(), false);
});

test('checkpoint streams snapshot metadata and order writes; successful older saves cannot clear newer typing', async () => {
  const f = fixture();
  f.owner.edit();
  const old = await f.owner.checkpoint();
  f.base.meta.title = 'new title';
  f.edit('new body');
  const latest = await f.owner.checkpoint();
  await f.owner.saved(old.revision);
  assert.equal(f.owner.unresolved, true);
  assert.equal(f.records.get(old.sequence).base.meta.title, 'Title');
  assert.ok(latest.sequence > old.sequence);
  await f.owner.saved(latest.revision);
  assert.equal(f.owner.unresolved, false);
});

test('lost, duplicate and reordered hints always reread without changing draft/raw or resolving recovery', async () => {
  const f = fixture();
  f.owner.conflict();
  await f.owner.checkpoint();
  const stop = f.watch();
  await new Promise(r => setTimeout(r, 0));
  for (const event of ['note-invalidated', 'note-invalidated', 'focus', 'visibilitychange', 'app-lifecycle-flush']) await f.hint(event);
  await f.tick(); // No event and unchanged data_version still triggers periodic revalidation.
  assert.ok(f.rereads >= 5);
  assert.equal(f.owner.unresolved, true);
  assert.equal(f.base.raw, 'base');
  assert.equal(f.memory.get(f.owner.owner).body, 'draft one');
  assert.equal(f.calls.some(([cmd]) => cmd === 'recovery_resolve'), false);
  stop();
});

test('unresolved transient blob bytes prevent a durable checkpoint and fork', async () => {
  const f = fixture();
  f.edit('![image](blob:missing)');
  await assert.rejects(f.owner.checkpoint());
  await assert.rejects(f.owner.fork());
  assert.equal(f.records.size, 0);
  assert.equal(f.forks.size, 0);
  assert.equal(f.owner.unresolved, true);
  assert.equal(f.memory.get(f.owner.owner).body, '![image](blob:missing)');
});

test('an exact successful note acknowledgment can retire a failed blob checkpoint without refetching a retired object URL', async () => {
  const f = fixture();
  f.edit('![image](blob:retired-after-successful-upload)');
  await assert.rejects(f.owner.checkpoint());
  assert.equal(f.owner.unresolved, true);
  // This hook is invoked by Editor only after its authoritative note save
  // acknowledged this exact local revision; journal failure alone never calls it.
  const revision = f.memory.get(f.owner.owner).revision;
  f.disk(true);
  await assert.rejects(f.owner.saved(revision));
  assert.equal(f.owner.unresolved, true);
  assert.equal(f.memory.has(f.owner.owner), true);
  f.disk(false);
  await f.owner.saved(revision);
  assert.equal(f.owner.unresolved, false);
  assert.equal(f.memory.has(f.owner.owner), false);
});

test('conflict classification uses typed native states and only the legacy filesystem prefix', () => {
  const f = fixture();
  assert.equal(f.isConflict({ kind: 'io', message: 'Save conflict: misleading disk error' }), false);
  assert.equal(f.isConflict({ kind: 'invalid_policy', message: 'conflict' }), false);
  for (const kind of ['changed', 'deleted', 'trashed', 'vault_replaced']) assert.equal(f.isConflict({ kind, message: 'anything' }), true);
  assert.equal(f.isConflict('Save conflict: note changed on disk.'), true);
  assert.equal(f.isConflict(new Error('Save conflict: note changed on disk.')), true);
  assert.equal(f.isConflict(new Error('File named conflict could not be opened')), false);
});

test('literal blob syntax in prose is preserved without treating it as a transient attachment', async () => {
  const f = fixture();
  f.edit('The URL scheme is `blob:example`; this is prose.');
  const record = await f.owner.checkpoint();
  assert.equal(record.blobs.length, 0);
  const result = await f.owner.fork();
  assert.equal(result.note.content, 'The URL scheme is `blob:example`; this is prose.');
});
