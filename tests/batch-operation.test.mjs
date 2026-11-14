import assert from 'node:assert/strict';
import test from 'node:test';
import { runBatchOperation } from '../src/lib/utils/batch-operation.ts';
test('mixed deletion results only acknowledge successful rows and preserve all failed paths',async()=>{
  const calls=[];
  const result=await runBatchOperation(['saved','conflict','saved','disk-full'],path=>{calls.push(path);if(path==='conflict')throw Error('changed elsewhere');if(path==='disk-full')return Promise.reject(Error('disk full'));return Promise.resolve();});
  assert.deepEqual(calls,['saved','conflict','disk-full']);assert.deepEqual([...result.succeeded],['saved']);assert.deepEqual(result.failed.map(f=>f.path),['conflict','disk-full']);assert.match(result.failed[0].error.message,/changed elsewhere/);
});
