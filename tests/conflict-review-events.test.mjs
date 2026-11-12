import assert from 'node:assert/strict';
import test from 'node:test';
import { reviewDomIds, handleReviewKey, createFocusIntents } from '../src/lib/utils/editor-events.ts';
const event=(key,extra={})=>({key,ctrlKey:false,metaKey:false,shiftKey:false,isComposing:false,prevented:0,stopped:0,
  preventDefault(){this.prevented++;},stopPropagation(){this.stopped++;},...extra});
test('review DOM targets are distinct across panes and webviews',()=>{
  const sets=['window-a-pane-1','window-a-pane-2','window-b-pane-1'].map(reviewDomIds);
  assert.equal(new Set(sets.flatMap(Object.values)).size,9);
  for(const ids of sets)assert.notEqual(ids.search,ids.result);
});
test('F3 and application shortcuts cannot escape a dialog into another pane',()=>{
  let advances=0,finds=0,closes=0;
  const actions={advance(reverse){advances+=reverse?-1:1;},find(){finds++;},close(){closes++;},canClose:()=>true};
  for(const key of ['F3','Escape','s','f','h']) {
    const e=event(key,{ctrlKey:key.length===1});assert.equal(handleReviewKey(e,true,false,actions),true);
    assert.equal(e.prevented,1);assert.equal(e.stopped,1);
  }
  assert.equal(advances,0);assert.equal(finds,0);assert.equal(closes,0);
  handleReviewKey(event('F3'),true,true,actions);handleReviewKey(event('F3',{shiftKey:true}),true,true,actions);
  handleReviewKey(event('f',{metaKey:true}),true,true,actions);handleReviewKey(event('Escape'),true,true,actions);
  assert.equal(advances,0);assert.equal(finds,1);assert.equal(closes,1);
  const outside=event('F3');assert.equal(handleReviewKey(outside,false,true,actions),false);assert.equal(outside.stopped,0);
});
test('window focus generations do not authorize an async result after focus moves',()=>{
  const windowA=createFocusIntents(),windowB=createFocusIntents();const a=windowA.capture(),b=windowB.capture();
  windowA.invalidate();assert.equal(a(),false);assert.equal(b(),true);
  const current=windowA.capture();windowB.invalidate();assert.equal(current(),true);assert.equal(b(),false);
});
