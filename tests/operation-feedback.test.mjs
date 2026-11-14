import assert from 'node:assert/strict';
import test from 'node:test';
import { get } from 'svelte/store';
import { operationMessage, reportOperationError, reportOperationSuccess, dismissOperation } from '../src/lib/stores/operation-feedback.ts';
test('late dismissal cannot erase a newer error, including a repeated identical failure',()=>{
  reportOperationSuccess('Saved');const success=get(operationMessage);
  reportOperationError('Rename failed',{message:'The note changed elsewhere.'});const failure=get(operationMessage);
  dismissOperation(success.id);assert.equal(get(operationMessage),failure);assert.equal(failure.kind,'error');assert.match(failure.text,/changed elsewhere/);
  reportOperationError('Rename failed',new Error('The note changed elsewhere.'));const newer=get(operationMessage);assert.notEqual(newer.id,failure.id);
  dismissOperation(failure.id);assert.equal(get(operationMessage),newer);dismissOperation(newer.id);assert.equal(get(operationMessage),null);
});
