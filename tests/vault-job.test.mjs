import assert from 'node:assert/strict';
import test from 'node:test';
import { waitForVaultJob } from '../src/lib/utils/vault-job.ts';

const tick = () => new Promise(resolve => setImmediate(resolve));

test('maintenance completion keeps its listener until the matching vault worker finishes', async () => {
  let deliver;
  let started = false;
  let stopped = 0;
  const job = waitForVaultJob('/vault', async handler => {
    deliver = handler;
    return () => stopped++;
  }, async () => { started = true; });
  let complete = false;
  void job.then(() => { complete = true; });
  await tick();
  assert.equal(started, true);
  deliver({ vault_path: '/other', success: true });
  await tick();
  assert.equal(complete, false);
  assert.equal(stopped, 0);
  deliver({ vault_path: '/vault', success: true });
  await job;
  assert.equal(stopped, 1);
});

test('worker failure and invocation failure both clean up the maintenance listener', async () => {
  let deliver;
  let stopped = 0;
  const job = waitForVaultJob('/vault', async handler => {
    deliver = handler;
    return () => stopped++;
  }, async () => {});
  await tick();
  deliver({ vault_path: '/vault', success: false, error: 'Restore rejected' });
  await assert.rejects(job, /Restore rejected/);
  assert.equal(stopped, 1);
  await assert.rejects(waitForVaultJob('/vault', async () => () => stopped++, async () => {
    throw new Error('Transport failed');
  }), /Transport failed/);
  assert.equal(stopped, 2);
});
