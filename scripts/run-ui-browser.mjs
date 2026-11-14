// Mounted component gate using an installed Chrome/Chromium and Node's built-in
// CDP transport. Never downloads a browser or uses the user's browser profile.
import { createServer } from 'vite';
import { config } from './ui-acceptance.mjs';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const root=await mkdtemp(join(tmpdir(),'ryuk-ui-browser-'));
const server=await createServer(config);
let browser,socket,passed=false;
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));
try {
  await server.listen();
  const executable=process.env.CHROME_BIN || 'google-chrome';
  browser=spawn(executable,['--headless=new','--disable-gpu','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${root}`,'about:blank'],{stdio:['ignore','ignore','pipe']});
  let launchError;browser.on('error',error=>{launchError=error;});
  let stderr='';browser.stderr.on('data',data=>{stderr+=data;});
  let port;
  for(let n=0;n<100;n++) {
    if(launchError)throw Error(`Cannot launch installed browser ${executable}: ${launchError.message}. Set CHROME_BIN; this gate never downloads tooling.`);
    if(browser.exitCode!==null)throw Error(`Browser exited: ${stderr}`);
    try {port=Number((await readFile(join(root,'DevToolsActivePort'),'utf8')).split('\n')[0]);break;}catch{await pause(100);}
  }
  if(!port)throw Error(`Browser debugging endpoint did not start: ${stderr}`);
  const target=await fetch(`http://127.0.0.1:${port}/json/new?about:blank`,{method:'PUT'}).then(r=>r.json());
  socket=new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve,reject)=>{socket.addEventListener('open',resolve,{once:true});socket.addEventListener('error',reject,{once:true});});
  let sequence=0;const pending=new Map();
  socket.addEventListener('message',event=>{const message=JSON.parse(String(event.data));if(!message.id)return;const pair=pending.get(message.id);if(!pair)return;pending.delete(message.id);message.error?pair.reject(Error(JSON.stringify(message.error))):pair.resolve(message.result);});
  const send=(method,params={})=>new Promise((resolve,reject)=>{const id=++sequence;pending.set(id,{resolve,reject});socket.send(JSON.stringify({id,method,params}));setTimeout(()=>{if(pending.delete(id))reject(Error(`CDP timeout: ${method}`));},10000).unref();});
  await send('Page.enable');await send('Runtime.enable');
  await send('Page.navigate',{url:'http://127.0.0.1:5189/tests/ui/index.html'});
  let status;
  for(let n=0;n<300;n++) {
    const result=await send('Runtime.evaluate',{expression:'document.documentElement.dataset.acceptance',returnByValue:true});status=result.result?.value;
    if(status==='passed'||status==='failed')break;
    await pause(200);
  }
  const result=await send('Runtime.evaluate',{expression:'document.getElementById("results")?.textContent',returnByValue:true});
  const report=result.result?.value||'Harness did not publish a result.';
  await writeFile(join(root,'result.txt'),report);console.log(report);
  if(status!=='passed') {
    const screenshot=await send('Page.captureScreenshot');await writeFile(join(root,'failure.png'),Buffer.from(screenshot.data,'base64'));
    throw Error(`Mounted UI gate ${status||'timed out'}. Artifacts: ${root}`);
  }
  passed=true;
} finally {
  socket?.close();
  if(browser && browser.exitCode===null)browser.kill('SIGTERM');
  await server.close();
  if(passed)await rm(root,{recursive:true,force:true});
  else console.error(`UI fixture artifacts retained at ${root}`);
}
