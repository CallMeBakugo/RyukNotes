import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import fs from 'node:fs';
import ts from 'typescript';
function bridge(events, timers={setTimeout,clearTimeout}, invoke=async()=>({path:'/fixture.rn',file:'original'})) {
 const exports={};
 vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/lib/utils/window-shortcuts.ts','utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,{exports,require:name=>name.includes('/core')?{invoke}:events,crypto:{randomUUID:()=> 'request-123'},...timers});
 return exports;
}
test('detached shortcut waits for an acknowledged, focused execution in the matching vault',async()=>{
 const listeners=new Map(),steps=[];
 const api=bridge({listen:async(name,handler)=>{listeners.set(name,handler);return()=>{steps.push('unlisten');listeners.delete(name);};},emitTo:async(target,name,payload)=>{
  if(name==='main-shortcut-request') {
   assert.equal(target,'main');
   await api.receiveMainShortcut(payload,'/fixture.rn',()=>true,async()=>{steps.push('focus');},action=>{steps.push(action);});
  } else {assert.equal(target,'note-fixture');listeners.get(name)?.({payload});}
 }});
 await api.requestMainShortcut('new-note','/fixture.rn','note-fixture');
 assert.deepEqual(steps,['focus','new-note','unlisten']);assert.equal(listeners.size,0);
});
test('wrong-vault and unknown forwarded shortcuts are refused with an error acknowledgment',async()=>{
 const replies=[];const api=bridge({emitTo:async(...args)=>replies.push(args)});
 for(const [action,vault] of [['new-note','/other.rn'],['save','/fixture.rn']]) await api.receiveMainShortcut({id:action,vault,action,replyWindow:'note-fixture'},'/fixture.rn',()=>true,()=>assert.fail('focus'),()=>assert.fail('execute'));
 assert.equal(replies.length,2);assert.ok(replies.every(reply=>reply[2].error));
});
test('a vault/window transition while focusing cannot execute a stale shortcut',async()=>{
 let current=true;const replies=[];const api=bridge({emitTo:async(...args)=>replies.push(args)});
 await api.receiveMainShortcut({id:'1',vault:'/fixture.rn',action:'new-note',replyWindow:'note-fixture'},'/fixture.rn',()=>current,async()=>{current=false;},()=>assert.fail('stale execute'));
 assert.match(replies[0][2].error,/changed/);
});
test('a missing main-window acknowledgment times out and removes its listener',async()=>{
 let timeout,unlistened=false;
 const api=bridge({listen:async()=>()=>{unlistened=true;},emitTo:async()=>{}},{setTimeout:fn=>{timeout=fn;return 1;},clearTimeout:()=>{}});
 const pending=api.requestMainShortcut('quick-open','/fixture.rn','note-fixture');
 const rejected=assert.rejects(pending,/acknowledge/);
 await new Promise(resolve=>setImmediate(resolve));timeout();await rejected;assert.equal(unlistened,true);
});

test('a replaced database file refuses a forwarded action even when its path is unchanged',async()=>{
 const replies=[];
 const api=bridge({emitTo:async(...args)=>replies.push(args)},undefined,async()=>({path:'/fixture.rn',file:'replacement'}));
 await api.receiveMainShortcut({id:'1',vault:'/fixture.rn',action:'new-note',replyWindow:'note-fixture',origin:{path:'/fixture.rn',file:'original'}},'/fixture.rn',()=>true,()=>assert.fail('focus'),()=>assert.fail('execute'));
 assert.match(replies[0][2].error,/replaced/);
});
