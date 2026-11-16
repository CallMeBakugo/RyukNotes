import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { Schema } from '@tiptap/pm/model';
import { Plugin, PluginKey, Selection, TextSelection, EditorState } from '@tiptap/pm/state';
const raw=fs.readFileSync('src/lib/components/Editor.svelte','utf8');
const ast=ts.createSourceFile('editor.ts',raw.slice(raw.indexOf('>')+1,raw.indexOf('</script>')),ts.ScriptTarget.Latest,true);
function extension(name,globals={}) {
 let found;
 function visit(node) {
  if(ts.isCallExpression(node)&&node.expression.getText(ast)==='Extension.create') {
   const config=node.arguments[0];
   if(config&&ts.isObjectLiteralExpression(config)&&config.properties.some(p=>ts.isPropertyAssignment(p)&&p.name.getText(ast)==='name'&&ts.isStringLiteral(p.initializer)&&p.initializer.text===name))found=node;
  }
  ts.forEachChild(node,visit);
 }
 visit(ast);assert.ok(found,name);
 const context=vm.createContext({Extension:{create:value=>value},Plugin,PluginKey,Selection,...globals});
 vm.runInContext(ts.transpileModule('this.extension='+found.getText(ast),{compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText,context);
 return context.extension;
}
const schema=new Schema({nodes:{doc:{content:'block+'},paragraph:{content:'text*',group:'block'},text:{group:'inline'},bulletList:{content:'listItem+',group:'block'},listItem:{content:'paragraph+'}}});
const paragraph=text=>schema.node('paragraph',null,schema.text(text));
function view(doc,position){const result={editable:true,state:EditorState.create({schema,doc,selection:TextSelection.create(doc,position)}),dispatch(transaction){this.state=this.state.apply(transaction);}};result.dispatch=result.dispatch.bind(result);return result;}
const arrow=(key,shift=false)=>({key,altKey:true,shiftKey:shift,preventDefault(){}});
test('the production WYSIWYG move shortcut moves whole blocks and preserves a valid caret',()=>{
 const plugin=extension('moveLineShortcuts').addProseMirrorPlugins()[0];
 const doc=schema.node('doc',null,[paragraph('one'),paragraph('two')]);const current=view(doc,6);
 assert.equal(plugin.props.handleDOMEvents.keydown(current,arrow('ArrowUp')),true);
 assert.equal(current.state.doc.firstChild.textContent,'two');
 assert.equal(current.state.selection.$from.parent.textContent,'two');
 plugin.props.handleDOMEvents.keydown(current,arrow('ArrowDown'));
 assert.equal(current.state.doc.firstChild.textContent,'one');
});
test('the production WYSIWYG list shortcut moves a list item rather than copying a line',()=>{
 const plugin=extension('moveLineShortcuts').addProseMirrorPlugins()[0];
 const doc=schema.node('doc',null,schema.node('bulletList',null,['one','two'].map(text=>schema.node('listItem',null,paragraph(text)))));
 const positions=[];doc.descendants((node,pos)=>{if(node.type.name==='listItem')positions.push(pos);});
 const current=view(doc,positions[1]+2);
 plugin.props.handleDOMEvents.keydown(current,arrow('ArrowUp',true));
 assert.equal(current.state.doc.firstChild.firstChild.textContent,'two');
 assert.equal(current.state.doc.firstChild.childCount,2);
 plugin.props.handleDOMEvents.keydown(current,arrow('ArrowDown',true));
 assert.equal(current.state.doc.firstChild.firstChild.textContent,'one');
});
test('the production strike and collapsible bindings invoke exactly their advertised commands',()=>{
 let strike=0,details=0;const readOnly={value:false};
 const keys=extension('strikeShortcut').addKeyboardShortcuts.call({editor:{commands:{toggleStrike:()=>{strike++;return true;}}}});
 assert.equal(keys['Mod-Shift-x'](),true);assert.equal(strike,1);
 const plugin=extension('collapsibleKeymap',{get:store=>store.value,readOnly,insertDetails:()=>details++}).addProseMirrorPlugins()[0];
 const event={ctrlKey:true,metaKey:false,altKey:false,shiftKey:false,code:'Period',key:'.',preventDefault(){}};
 assert.equal(plugin.props.handleDOMEvents.keydown({editable:true},event),true);assert.equal(details,1);
 readOnly.value=true;assert.equal(plugin.props.handleDOMEvents.keydown({editable:true},event),false);assert.equal(details,1);
});

test('custom move and collapsible shortcuts cannot edit a read-only or viewer document',()=>{
 const doc=schema.node('doc',null,[paragraph('one'),paragraph('two')]);const current=view(doc,6);current.editable=false;
 const mover=extension('moveLineShortcuts').addProseMirrorPlugins()[0];
 assert.equal(mover.props.handleDOMEvents.keydown(current,arrow('ArrowUp')),false);
 assert.equal(mover.props.handleDOMEvents.keydown(current,arrow('ArrowUp',true)),false);
 assert.equal(current.state.doc,doc);
 const details=extension('collapsibleKeymap',{get:()=>false,readOnly:{},insertDetails:()=>assert.fail('read-only insert')}).addProseMirrorPlugins()[0];
 assert.equal(details.props.handleDOMEvents.keydown({editable:false},{ctrlKey:true,metaKey:false,altKey:false,shiftKey:false,code:'Period',preventDefault(){}}),false);
});
