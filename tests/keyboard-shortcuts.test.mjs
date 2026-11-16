import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { writable, get } from 'svelte/store';

function loadBindings(platform='Linux', stored=null) {
 const exports={}, listeners={}; let saved=stored;
 const context={exports,require:()=>({writable}),navigator:{platform},localStorage:{getItem:key=>key==='ryuk-keybindings'?saved:null,setItem:(_,value)=>{saved=value;}},window:{addEventListener:(name,fn)=>listeners[name]=fn}};
 vm.runInNewContext(ts.transpileModule(fs.readFileSync('src/lib/keybindings.ts','utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText,context);
 return {...exports,listeners,replaceStorage:value=>{saved=value;listeners.storage({key:'ryuk-keybindings'});}};
}
const event=(binding,extra={})=>({code:binding.code,key:'',ctrlKey:!!binding.mod,metaKey:false,shiftKey:!!binding.shift,altKey:!!binding.alt,prevented:0,stopped:0,preventDefault(){this.prevented++;},stopPropagation(){this.stopped++;},...extra});
const plain=value=>JSON.parse(JSON.stringify(value));
function component(path,globals,names=['runShortcut','handleKeydown']) {
 const source=fs.readFileSync(path,'utf8');
 const ast=ts.createSourceFile('component.ts',source.slice(source.indexOf('>')+1,source.indexOf('</script>')),ts.ScriptTarget.Latest,true);
 const functions=names.map(name=>ast.statements.find(node=>ts.isFunctionDeclaration(node)&&node.name?.text===name));
 assert.ok(functions.every(Boolean));
 const context=vm.createContext({...globals});
 vm.runInContext(ts.transpileModule(functions.map(fn=>fn.getText(ast)).join('\n')+`\nthis.functions={${names.join(',')}}`,{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText,context);
 return context;
}
for(const platform of ['Linux','MacIntel']) {
 test(`${platform}: every default resolves exactly once, shifted keys never alias unshifted keys`,()=>{
  const kb=loadBindings(platform);
  for(const action of kb.ACTIONS) assert.equal(kb.matchAction(event(action.default),kb.DEFAULT_BINDINGS),action.id);
  assert.equal(kb.matchAction(event({mod:true,shift:true,code:'KeyS'}),kb.DEFAULT_BINDINGS),null);
  assert.equal(kb.DEFAULT_BINDINGS['replace-in-note'].alt,platform==='MacIntel'?true:undefined);
 });
}
test('reset swaps an occupied default without turning Save into New note',()=>{
 const kb=loadBindings();
 assert.equal(kb.setBinding('save',{mod:true,alt:true,code:'KeyS'}),true);
 assert.equal(kb.setBinding('new-note',{mod:true,code:'KeyS'}),true);
 kb.resetBinding('save');
 const map=get(kb.keybindings);
 assert.equal(kb.matchAction(event({mod:true,code:'KeyS'}),map),'save');
 assert.equal(kb.matchAction(event({mod:true,alt:true,code:'KeyS'}),map),'new-note');
 assert.equal(new Set(Object.values(map).map(b=>JSON.stringify(plain(b)))).size,kb.ACTIONS.length);
});
test('rebindings reject collisions, editor commands, typing keys and unknown actions',()=>{
 const kb=loadBindings();
 for(const binding of [{mod:true,code:'KeyS'},{mod:true,code:'KeyB'},{mod:true,code:'Period'},{alt:true,shift:true,code:'ArrowUp'},{code:'KeyA'},{code:'Enter'},{mod:'yes',code:'KeyQ'},{mod:true,code:'ControlLeft'}]) assert.equal(kb.setBinding('new-note',binding),false,JSON.stringify(binding));
 assert.equal(kb.setBinding('__proto__',{mod:true,code:'KeyQ'}),false);
 assert.equal(kb.setBinding('save',{mod:true,alt:true,code:'KeyS'}),true);
});
test('corrupt persisted maps and cross-window changes cannot leave duplicate or reserved bindings',()=>{
 const kb=loadBindings('Linux',JSON.stringify({'new-note':{mod:true,code:'KeyS'},save:{mod:true,code:'KeyS'},'toggle-theme':{mod:true,code:'KeyB'},'find-in-note':null}));
 assert.equal(kb.matchAction(event({mod:true,code:'KeyS'}),get(kb.keybindings)),'save');
 assert.equal(kb.matchAction(event({mod:true,code:'KeyB'}),get(kb.keybindings)),null);
 kb.replaceStorage(JSON.stringify({save:{mod:true,alt:true,code:'KeyS'}}));
 assert.equal(kb.matchAction(event({mod:true,alt:true,code:'KeyS'}),get(kb.keybindings)),'save');
 kb.replaceStorage('broken JSON');
 assert.equal(kb.matchAction(event({mod:true,code:'KeyS'}),get(kb.keybindings)),'save');
});
test('typing, IME, held keys and dialogs do not dispatch application shortcuts',()=>{
 const kb=loadBindings();
 for(const extra of [{isComposing:true},{keyCode:229},{repeat:true},{defaultPrevented:true},{key:'Dead'},{key:'Process'},{target:{closest:()=>({})}}]) assert.equal(kb.shouldHandleShortcut(event({mod:true,code:'KeyN'},extra)),false);
 assert.equal(kb.shouldHandleShortcut(event({mod:true,code:'KeyN'})),true);
});
function fixture(detached=false) {
 const kb=loadBindings(),calls=[];
 const log=name=>(...args)=>{calls.push([name,...args]);return Promise.resolve(true);};
 const globals={...kb,$keybindings:kb.DEFAULT_BINDINGS,$activeNotePath:'note-id',$activeNote:{meta:{title:'Note'}},$viewerNote:null,$sourceMode:false,$readOnly:false,$sidebarCollapsed:false,$focusMode:false,$theme:'light',$resolvedTheme:'light',$customThemes:[],$showInfo:false,$showSettings:false,$showSearch:false,$showCommandPalette:false,sessionReady:true,stateReady:true,ownerIsCurrent:()=>true,disposed:false,ownerVault:'/fixture.rn',darkThemes:['dark'],
 editor:Object.fromEntries(['forceSave','openNoteSearch','addLinkFromToolbar','moveOpenNoteToTrash'].map(name=>[name,log(name)])),navigateHistory:log('history'),createAndFocusNote:log('new'),toggleNoteList:log('notelist'),setTheme:log('theme'),openNoteWindow:log('window'),requestMainShortcut:log('forward'),MAIN_WINDOW_ACTIONS:new Set(['new-note','quick-open','search-vault','toggle-sidebar','toggle-notelist','toggle-focus','nav-back','nav-forward']),appWindow:{label:'note-fixture',isFullscreen:async()=>false,setFullscreen:log('fullscreen')},reportOperationError:log('error')};
 return {kb,calls,context:component(`src/lib/components/${detached?'NoteWindow':'AppLayout'}.svelte`,globals)};
}
test('every advertised application action executes its main-window behavior',async()=>{
 const expected={'new-note':'new',save:'forceSave','find-in-note':'openNoteSearch','replace-in-note':'openNoteSearch','toggle-theme':'theme','toggle-notelist':'notelist','nav-back':'history','nav-forward':'history',fullscreen:'fullscreen','open-new-window':'window'};
 for(const action of loadBindings().ACTIONS) {
  const {context,calls}=fixture();await context.functions.runShortcut(action.id);
  if(expected[action.id])assert.equal(calls[0]?.[0],expected[action.id],action.id);
  else {const store={'quick-open':'$showCommandPalette','search-vault':'$showSearch','toggle-sidebar':'$sidebarCollapsed','toggle-focus':'$focusMode','toggle-readonly':'$readOnly','toggle-source':'$sourceMode'}[action.id];assert.ok(store,action.id);assert.equal(context[store],true,action.id);}
  if(action.id==='replace-in-note')assert.equal(calls[0][1],true);
 }
});
test('detached windows execute every note-local shortcut and route all main-window actions',async()=>{
 for(const action of loadBindings().ACTIONS) {
  const {context,calls}=fixture(true);await context.functions.runShortcut(action.id);
  if(context.MAIN_WINDOW_ACTIONS.has(action.id))assert.deepEqual(calls[0],['forward',action.id,'/fixture.rn','note-fixture',undefined]);
  else if(['toggle-source','toggle-readonly'].includes(action.id))assert.equal(context[action.id==='toggle-source'?'$sourceMode':'$readOnly'],true);
  else assert.ok(calls.length,action.id);
 }
});
test('application shortcuts are captured once; fixed formatting keys remain editor-owned',async()=>{
 for(const detached of [false,true]) {
  const {context,calls,kb}=fixture(detached);
  const save=event(kb.DEFAULT_BINDINGS.save);context.functions.handleKeydown(save);await Promise.resolve();
  assert.equal(save.prevented,1);assert.equal(save.stopped,1);assert.equal(calls[0][0],'forceSave');
  const repeated=event(kb.DEFAULT_BINDINGS.save,{repeat:true});context.functions.handleKeydown(repeated);assert.equal(repeated.prevented,1);assert.equal(calls.length,1);
  const bold=event({mod:true,code:'KeyB'});context.functions.handleKeydown(bold);assert.equal(bold.prevented,0);
  const link=event({mod:true,code:'KeyK'});context.functions.handleKeydown(link);assert.ok(calls.some(([name])=>name==='addLinkFromToolbar'));
  const before=calls.length;context.functions.handleKeydown(event(kb.DEFAULT_BINDINGS.save,{isComposing:true}));assert.equal(calls.length,before);
 }
});
