// Run HOST-SIDE against a Tauri-built CLI. Never selects a real vault/config.
// This is CLI process acceptance, not proof of the human two-webview gate.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
if (!process.argv[2]) throw Error('Usage: node scripts/stage4f-host.mjs /absolute/path/to/tauri-built/ryuk');
const binary=resolve(process.argv[2]),root=mkdtempSync(join(tmpdir(),'ryuk-stage4f-'));
const vault=join(root,'acceptance.rn');
const env={...process.env,RYUK_V3_LEDGER:join(root,'ledger'),XDG_CONFIG_HOME:join(root,'config'),XDG_DATA_HOME:join(root,'data'),RYUK_VAULT:vault};
mkdirSync(env.XDG_CONFIG_HOME,{recursive:true});mkdirSync(env.XDG_DATA_HOME,{recursive:true});
const log=[];
function cli(args,input='') {
  const result=spawnSync(binary,['--vault',vault,...args],{env,input,encoding:'utf8'});
  if(result.error)throw result.error;
  log.push({args,status:result.status,stderr:result.stderr});return result;
}
function success(args,input=''){const result=cli(args,input);assert.equal(result.status,0,result.stderr);return result.stdout.trim();}
function race(args,input) {return new Promise((res,rej)=>{
  const child=spawn(binary,['--vault',vault,...args],{env,stdio:['pipe','pipe','pipe']});let stdout='',stderr='';
  child.stdout.on('data',s=>stdout+=s);child.stderr.on('data',s=>stderr+=s);child.on('error',rej);child.on('close',status=>{log.push({args,status,stderr});res({status,stdout,stderr});});child.stdin.end(input);
});}
try {
  const id=success(['notes','new','Acceptance fixture','--stdin'],'base 😀\r\n');
  let current=JSON.parse(success(['notes','show',id,'--json']));assert.ok(current.accepted.token.startsWith('revision-v3.'));
  const captured=current.accepted.token;
  const outcomes=await Promise.all(['writer A\r\n','writer B\r\n'].map(body=>race(['notes','update',id,'--expected-token',captured,'--stdin'],body)));
  assert.deepEqual(outcomes.map(o=>o.status).sort(),[0,3],'one writer wins, the stale writer exits conflict 3');
  current=JSON.parse(success(['notes','show',id,'--json']));assert.ok(['writer A\r\n','writer B\r\n'].includes(current.content));
  const body=current.content;
  success(['notes','tag',id,'--expected-token',current.accepted.token,'--add','host-acceptance']);
  const tagged=JSON.parse(success(['notes','show',id,'--json']));assert.equal(tagged.content,body);assert.ok(tagged.tags.includes('host-acceptance'));
  assert.equal(cli(['notes','update',id,'--expected-token',current.accepted.token,'--stdin'],'stale').status,3);
  success(['notes','update',id,'--expected-token',tagged.accepted.token,'--stdin'],'base 😀\r\n');
  assert.equal(cli(['notes','update',id,'--expected-token',captured,'--stdin'],'ABA overwrite').status,3);
  writeFileSync(join(root,'acceptance.json'),JSON.stringify({binary,vault,env:{RYUK_V3_LEDGER:env.RYUK_V3_LEDGER,XDG_CONFIG_HOME:env.XDG_CONFIG_HOME,XDG_DATA_HOME:env.XDG_DATA_HOME},cli:'passed',gui:'NOT RUN',log},null,2));
  console.log(`CLI process gates passed. Artifacts retained at ${root}.`);
  console.log('Launch the Tauri-built GUI with the same three environment paths from acceptance.json and open the disposable vault. Follow docs/revision-ancestry-stage4f.md for migration, two-webview, focus, restore and restart. CLI success does not mark those gates passed.');
} catch(error) {writeFileSync(join(root,'failure.json'),JSON.stringify({binary,vault,error:String(error),log},null,2));console.error(`FAILED; retained artifacts at ${root}`);throw error;}
