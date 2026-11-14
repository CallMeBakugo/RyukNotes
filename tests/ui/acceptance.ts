import { mount, unmount, tick } from 'svelte';
import ReviewHarness from './ReviewHarness.svelte';
import RecoveryInbox from '../../src/lib/components/RecoveryInbox.svelte';
import OperationFeedback from '../../src/lib/components/OperationFeedback.svelte';
import { ConflictReviewModel } from '../../src/lib/utils/conflict-review';
import { reportOperationError, reportOperationSuccess, operationMessage } from '../../src/lib/stores/operation-feedback';
import { recordVaultOpenStatus } from '../../src/lib/utils/vault-open-status';
const output=document.getElementById('results')!, target=document.getElementById('fixture')!;
const lines:string[]=[];
function assert(ok:unknown,message:string) { if(!ok)throw Error(message);lines.push(`PASS ${message}`);output.textContent=lines.join('\n'); }
const settle=async()=>{await tick();await new Promise(r=>setTimeout(r,40));await tick();};
const note=(body:string,token:string)=>({path:'n',content:body,raw:body,meta:{id:'n',title:'Title',tags:[],pinned:false,created:'date',modified:'date',accepted:{token,vault:{path:'/fixture.rn',file:'fixture'},body,title:'Title',tags:[],pinned:false,trashed:false,created:'date',modified:'date'}}});
try {
  let localRevision=1;
  const models=[0,1].map(index=>new ConflictReviewModel({
    open:async()=>({available:true,reviewId:`review-${index}`,revision:localRevision,base:note('base\n','B'),local:note('local\n','B'),remote:note('remote\n','R')}),
    checkpoint:async()=>{},cancel:async()=>{},apply:async()=>note('applied\n','saved'),localRevision:()=>localRevision,ownerIsCurrent:()=>true,adopt:async()=>true,
  }));
  const harness=mount(ReviewHarness,{target,props:{models}});
  harness.activate(0);harness.renderer('first');await Promise.all(models.map(m=>m.open()));await settle();
  assert(target.querySelectorAll('[role="dialog"]').length===2,'two real conflict review components mount');
  const ids=[...target.querySelectorAll('[id]')].map(el=>el.id);assert(new Set(ids).size===ids.length,'review and nested history IDs are unique across mounted panes');
  for(const el of target.querySelectorAll('[aria-labelledby]'))for(const id of el.getAttribute('aria-labelledby')!.split(' '))assert(!!document.getElementById(id),`ARIA target exists: ${id}`);
  const snapshotTabs=[...target.querySelectorAll('[id$="-history-tab-snapshot"]')] as HTMLButtonElement[];snapshotTabs.forEach(tab=>tab.click());await settle();
  assert(target.textContent!.includes('first:'),'mounted snapshot view renders through the live renderer');
  harness.renderer('updated');await settle();assert(target.textContent!.includes('updated:'),'new renderer callback reaches an already mounted review');
  assert(!target.textContent!.includes('first:'),'already mounted review drops the retired renderer output');
  const pane1=target.querySelector('[data-test-pane="1"]')!, input=pane1.querySelector('input')!;
  input.focus();const focusBefore=document.activeElement;input.dispatchEvent(new KeyboardEvent('keydown',{key:'F3',bubbles:true,cancelable:true}));await settle();assert(document.activeElement===focusBefore,'inactive review F3 cannot steal focus into another pane');
  localRevision++;models[0].invalidate('typing during review');await settle();assert(models[0].state.status==='stale','typing during mounted review fences apply');
  const cancel=[...target.querySelectorAll('[data-test-pane="0"] button')].find(el=>el.textContent==='Cancel review') as HTMLButtonElement;cancel.click();await settle();assert(!target.querySelector('[data-test-pane="0"]'),'cancel unmounts its own review only');assert(!!target.querySelector('[data-test-pane="1"]'),'other review survives cancellation');
  await models[0].open();await settle();assert(!!target.querySelector('[data-test-pane="0"]'),'canceled review can reopen');
  await Promise.all(models.map(m=>m.close()));await unmount(harness);await settle();
  // Fake only the native transport. The real inbox component, events, Svelte
  // effects, dismissal and DOM lifecycle run in the browser.
  let callback=0;
  (window as any).__TAURI_INTERNALS__={transformCallback:()=>++callback,unregisterCallback:()=>{},invoke:async(command:string)=>command==='recovery_list'||command==='recovery_orphan_pins'?[]:0};
  recordVaultOpenStatus('Fixture migration completed');
  const inbox=mount(RecoveryInbox,{target,props:{notice:'Fixture barrier refusal'}});await settle();assert(target.textContent!.includes('Fixture barrier refusal'),'barrier notice renders its actual message');
  (target.querySelector('[aria-label="Dismiss vault status"]') as HTMLButtonElement).click();
  (target.querySelector('[aria-label="Dismiss recovery notice"]') as HTMLButtonElement).click();await settle();assert(!target.querySelector('.recovery-inbox'),'dismissing status-only notices removes the empty inbox');await unmount(inbox);
  const feedback=mount(OperationFeedback,{target});reportOperationError('Fixture action','Expected refusal');await settle();assert(!!target.querySelector('[role="alert"]'),'operation failures are visible');
  (target.querySelector('button') as HTMLButtonElement).click();await settle();assert(!target.querySelector('[role="alert"]'),'error status is dismissible');
  reportOperationSuccess('Fixture success');await settle();assert(!!target.querySelector('[role="status"]'),'success is announced');await new Promise(r=>setTimeout(r,5100));await settle();assert(!target.querySelector('[role="status"]'),'success expires without leaving a banner');operationMessage.set(null);await unmount(feedback);
  output.textContent=`ALL MOUNTED CHECKS PASSED\n${lines.join('\n')}`;
  document.documentElement.dataset.acceptance='passed';
} catch(error) { output.textContent=`FAILED: ${String(error)}\n${lines.join('\n')}`;document.documentElement.dataset.acceptance='failed';console.error(error); }
