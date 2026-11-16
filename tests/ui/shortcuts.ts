import { mount, unmount, tick } from 'svelte';
import { get } from 'svelte/store';
import InfoPanel from '../../src/lib/components/InfoPanel.svelte';
import { showInfo } from '../../src/lib/stores/app';
import { keybindings, setBinding, matchAction } from '../../src/lib/keybindings';
import { SourceController } from '../../src/lib/editor/source/controller';
import { Editor } from '@tiptap/core';
import StarterKit from '@tiptap/starter-kit';
import Underline from '@tiptap/extension-underline';

type Check = (condition: unknown, message: string) => void;
function key(target: HTMLElement, code: string, value: string, options: KeyboardEventInit = {}) {
 const mac=navigator.platform.startsWith('Mac');
 target.dispatchEvent(new KeyboardEvent('keydown',{key:value,code,keyCode:code.startsWith('Key')?code.charCodeAt(3):0,bubbles:true,cancelable:true,ctrlKey:!mac,metaKey:mac,...options}));
}
export async function checkShortcuts(target: HTMLElement, check: Check) {
 const initial=get(keybindings); const originalInfo=get(showInfo);
 showInfo.set(true);
 const info=mount(InfoPanel,{target}); await tick(); await tick();
 const button=(label: string)=> {
  const row=[...target.querySelectorAll<HTMLElement>('.shortcut-row')].find(row=>row.querySelector('.shortcut-desc')?.textContent===label);
  const control=row?.querySelector<HTMLButtonElement>('button'); if(!control)throw new Error('Missing shortcut: '+label);return control;
 };
 try {
  check(target.textContent?.includes('Command palette'),'palette shortcut has an accurate label');
  button('Save').click(); await tick();
  key(button('Save'),'KeyB','b');await tick();
  check(target.textContent?.includes('Reserved editor shortcut'),'shortcut dialog refuses editor-reserved bindings');
  check(setBinding('save',{mod:true,alt:true,code:'KeyS'}),'save can use a free custom shortcut');
  check(setBinding('new-note',{mod:true,code:'KeyS'}),'the freed shortcut can be reassigned');await tick();
  button('Save').dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true})); await tick();
  check(matchAction(new KeyboardEvent('keydown',{code:'KeyS',ctrlKey:true}),get(keybindings))==='save','right-click reset restores Save without a collision');
  check(target.textContent?.includes('WYSIWYG formatting')&&target.textContent?.includes('Source editor'),'shortcut descriptions identify their editor modes');
 } finally {keybindings.set(initial);showInfo.set(originalInfo);await unmount(info);}

 const richHost=document.createElement('div');target.append(richHost);
 const rich=new Editor({element:richHost,extensions:[StarterKit.configure({underline:false}),Underline],content:'<p>hello</p>'});
 try {
  for(const [code,letter,mark] of [['KeyB','b','bold'],['KeyI','i','italic'],['KeyU','u','underline'],['KeyE','e','code']]) {
   rich.commands.setContent('<p>hello</p>');rich.commands.setTextSelection({from:1,to:6});rich.view.focus();
   key(rich.view.dom,code,letter);
   check(rich.getJSON().content?.[0].content?.[0].marks?.some(value=>value.type===mark),`${mark} keyboard shortcut changes the real rich-text document`);
  }
 } finally {rich.destroy();richHost.remove();}
 const historyHost=document.createElement('div');target.append(historyHost);
 const history=new Editor({element:historyHost,extensions:[StarterKit],content:'<p>hello</p>'});
 try {
  history.commands.setTextSelection(6);history.commands.insertContent('!');history.view.focus();
  key(history.view.dom,'KeyZ','z');check(history.getText()==='hello','rich-text keyboard undo restores the prior document');
  key(history.view.dom,'KeyZ','Z',{shiftKey:true});check(history.getText()==='hello!','rich-text keyboard redo reapplies the edit');
 } finally {history.destroy();historyHost.remove();}
 const sourceHost=document.createElement('div');target.append(sourceHost);
 const source=new SourceController();source.load('one\ntwo');const detach=source.attach(sourceHost);
 try {
  source.select(4);source.view!.focus();
  key(source.view!.contentDOM,'ArrowUp','ArrowUp',{ctrlKey:false,metaKey:false,altKey:true});
  check(source.state.doc.toString()==='two\none','source keyboard moves a line');
  key(source.view!.contentDOM,'ArrowDown','ArrowDown',{ctrlKey:false,metaKey:false,altKey:true,shiftKey:true});
  check(source.state.doc.lines===3,'source keyboard duplicates a line');
  source.load('hello');source.select(5);source.replace('!',5,5);source.view!.focus();
  key(source.view!.contentDOM,'KeyZ','z');check(source.state.doc.toString()==='hello','source keyboard undo restores the prior document');
  key(source.view!.contentDOM,'KeyZ','Z',{shiftKey:true});check(source.state.doc.toString()==='hello!','source keyboard redo reapplies the edit');
 } finally {detach();sourceHost.remove();}
}
