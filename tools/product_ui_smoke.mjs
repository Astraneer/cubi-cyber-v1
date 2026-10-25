#!/usr/bin/env node
// Real product canvas/input, durable authority, local import privacy and F8.
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {mkdir,mkdtemp,readFile,writeFile,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {withBrowser,pngPixels,pause,root} from './browser.mjs';
await mkdir(join(root,'.local/tmp'),{recursive:true});
const folder=await mkdtemp(join(root,'.local/tmp/product-authority-'));
const ready=join(folder,'ready'),pack=join(folder,'private.zip'),minecraft=join(folder,'minecraft/.minecraft'),downloads=join(folder,'downloads');
const canary=`PRIVATE_PRODUCT_RESOURCE_${process.pid}_NEVER_UPLOAD`;
const fixture=spawnSync('python3',['-c',`import sys,pathlib,zipfile,hashlib,json
with zipfile.ZipFile(sys.argv[1],'w') as z:z.writestr('CREDITS.md',sys.argv[2])
root=pathlib.Path(sys.argv[3]);sound=pathlib.Path('donor/web/sounds/ui/click_1.ogg').read_bytes();h=hashlib.sha1(sound).hexdigest()
(root/'assets/indexes').mkdir(parents=True);(root/'assets/objects'/h[:2]).mkdir(parents=True)
(root/'assets/objects'/h[:2]/h).write_bytes(sound)
(root/'assets/indexes/test.json').write_text(json.dumps({'private':sys.argv[2],'objects':{'minecraft/sounds/random/click.ogg':{'hash':h,'size':len(sound)}}}))
(root/'launcher_accounts.json').write_text(sys.argv[2]+'PRIVATE_ACCOUNT_NEVER_READ')
`,pack,canary,minecraft],{cwd:root,encoding:'utf8'});assert.equal(fixture.status,0,fixture.stderr);await mkdir(downloads);
let log='';const server=spawn(join(root,'target/debug/server'),['--data',folder,'--ws','127.0.0.1:0','--udp','127.0.0.1:0','--ready-file',ready,'--spawn','8.5,70,8.5'],{cwd:root,stdio:['ignore','pipe','pipe']});
server.stdout.on('data',d=>log+=d);server.stderr.on('data',d=>log+=d);
try{
  let address;for(let i=0;i<200;i++){try{address=(await readFile(ready,'utf8')).match(/ws=(\S+)/)?.[1];}catch{}if(address)break;if(server.exitCode!==null)throw Error(log);await pause(100);}assert(address,log);
  await withBrowser({width:780,height:437,path:'/'},async({call,evaluate,waitFor,onEvent})=>{
    await waitFor('document.documentElement?.dataset.app',s=>s==='ready');
    const status=()=>evaluate('window.__M2Stats()');const ui=()=>evaluate('window.__CUBI.product_ui_stats()');
    const key=async(code,key=code)=>{await call('Input.dispatchKeyEvent',{type:'keyDown',code,key});await call('Input.dispatchKeyEvent',{type:'keyUp',code,key});};
    const type=async text=>{for(const char of text)await key(char>='0'&&char<='9'?`Digit${char}`:char==='.'?'Period':char===':'?'Semicolon':'KeyX',char);};
    const click=async(x,y)=>{await call('Input.dispatchMouseEvent',{type:'mouseMoved',x,y});await call('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x,y,button:'left',clickCount:1});await pause(150);};
    async function capture(name){await pause(100);const data=await evaluate("document.querySelector('canvas').toDataURL('image/png')");const bytes=Buffer.from(data.split(',')[1],'base64');const image=pngPixels(bytes);let bright=0;for(let y=0;y<437;y++)for(let x=0;x<780;x++){const p=image.sample(x,y);if(p[0]>200&&p[1]>190&&p[2]>170)bright++;}assert(bright>1000,`${name}: missing UI pixels ${bright}`);await mkdir(join(root,'.local/artifacts'),{recursive:true});await writeFile(join(root,'.local/artifacts',`product-${name}.png`),bytes);return bright;}
    assert.equal((await status()).uiScreen,'Welcome');assert.equal((await status()).connected,false);const welcomePixels=await capture('welcome');
    const before=(await status()).submittedFrames;await pause(300);assert((await status()).submittedFrames-before<=1,'unchanged Welcome keeps submitting');
    await click(295,292);assert.equal((await status()).uiScreen,'Settings(Graphics)');await click(475,78);assert.equal((await status()).uiScreen,'Settings(Sound)');await click(700,123);await key('Escape','Escape');
    await click(350,200);for(let i=0;i<30;i++)await key('Backspace','Backspace');await type(address);
    let socketFrames=0;const unsubscribe=onEvent('Network.webSocketFrameSent',()=>socketFrames++);await call('Network.enable');
    await click(350,250);await waitFor('window.__M2Stats()',s=>s.connected&&s.playerSpawned,120000);await waitFor('window.__M2Stats()',s=>s.uiScreen==='Game',120000);
    assert.equal((await status()).interactionMode,'authoritative-remote');assert(socketFrames>0);
    await waitFor('window.__CUBI.product_ui_stats()',s=>s.players===1&&s.pingMeasured,10000);
    await key('KeyE','e');await waitFor('window.__CUBI.product_ui_stats()',s=>s.screen==='Inventory'&&s.windowId!=='0000000000000000');assert.equal((await ui()).inventoryItems,0);const inventoryPixels=await capture('inventory');
    await key('KeyE','e');await waitFor('window.__CUBI.product_ui_stats()',s=>s.screen==='Game'&&s.windowId==='0000000000000000');
    const chat='Привет из настоящего клиента';await key('KeyT','t');await type(chat);await key('Enter','Enter');await waitFor('window.__CUBI.product_ui_stats()',s=>s.lastChat.endsWith(chat));assert.equal((await ui()).chatMessages,1);
    await call('Input.dispatchKeyEvent',{type:'keyDown',code:'Tab',key:'Tab'});const rosterPixels=await capture('roster');await call('Input.dispatchKeyEvent',{type:'keyUp',code:'Tab',key:'Tab'});await key('F3','F3');await pause(350);const profilerPixels=await capture('profiler');await key('F3','F3');
    await key('Escape','Escape');await click(390,182);assert.equal((await ui()).screen,'Resources');
    let privateRequests=0,leakedFrames=0,privateFrames=0;const requestOff=onEvent('Network.requestWillBeSent',()=>privateRequests++);const framesOff=onEvent('Network.webSocketFrameSent',event=>{privateFrames++;const data=event.response.opcode===2?Buffer.from(event.response.payloadData,'base64').toString('utf8'):event.response.payloadData;if(data.includes(canary)||data.includes('PRIVATE_ACCOUNT_NEVER_READ'))leakedFrames++;});
    await call('Page.setInterceptFileChooserDialog',{enabled:true});let chooser;const chooserOff=onEvent('Page.fileChooserOpened',event=>chooser=event);
    async function choose(x,y,path){chooser=undefined;await click(x,y);const until=Date.now()+10000;while(!chooser&&Date.now()<until)await pause(20);assert(chooser,'actual file picker missing');await call('DOM.setFileInputFiles',{files:[path],backendNodeId:chooser.backendNodeId});}
    await choose(390,152,pack);await waitFor('window.__CUBI.local_resource_summary()',s=>JSON.parse(s).files===1);await waitFor('window.__M2Stats()',s=>s.uiStatus==='Локальные звуки подготовлены');assert((await status()).connected);
    await choose(390,202,minecraft);const imported=JSON.parse(await waitFor('window.__CUBI.local_resource_summary()',s=>JSON.parse(s).revision===2));assert.equal(imported.files,2);assert.equal(imported.sounds,1);await waitFor('window.__M2Stats()',s=>s.uiStatus==='Локальные звуки подготовлены');assert((await status()).connected);
    await pause(300);requestOff();framesOff();chooserOff();assert.equal(privateRequests,0);assert.equal(leakedFrames,0);assert(privateFrames>0,'privacy test had no active game traffic');
    await call('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:downloads,eventsEnabled:true});await key('F8','F8');let report;for(let i=0;i<100;i++){try{report=JSON.parse(await readFile(join(downloads,'cubi-flight-recorder.json'),'utf8'));break;}catch{}await pause(50);}assert(report?.frames.length>0,'F8 local recorder file missing');assert(report.frames.some(frame=>frame.cpuNs>0));assert.equal(report.replayAvailable,false);assert(!JSON.stringify(report).includes(canary));await writeFile(join(root,'.local/artifacts/product-flight-recorder.json'),JSON.stringify(report,null,2)+'\n');
    await click(390,388);assert.equal((await ui()).screen,'Pause');await click(390,265);await waitFor('window.__M2Stats()',s=>s.uiScreen==='Welcome'&&!s.connected);
    const identity=await evaluate("Object.keys(localStorage).filter(key=>key.startsWith('cubi.identity.')).map(key=>localStorage.getItem(key).split(':')[0]).join(',')");assert(identity);
    await click(350,250);await waitFor('window.__M2Stats()',s=>s.connected&&s.uiScreen==='Game',120000);const sameIdentity=await evaluate(`Object.keys(localStorage).filter(key=>key.startsWith('cubi.identity.')).map(key=>localStorage.getItem(key).split(':')[0]).join(',')===${JSON.stringify(identity)}`);assert(sameIdentity,'resume replaced authority-issued identity');
    await key('Escape','Escape');await click(390,265);await waitFor('window.__M2Stats()',s=>s.uiScreen==='Welcome'&&!s.connected);unsubscribe();
    const result={welcomePixels,inventoryPixels,rosterPixels,profilerPixels,socketFrames,actualDefaultWelcome:true,actualAuthority:true,guardedInventory:true,authoritativeChat:true,serverMeasuredPing:true,privacy:{privateRequests,leakedFrames,activeFrames:privateFrames,importedFiles:imported.files,accountsExcluded:true},f8:{frames:report.frames.length,replayAvailable:false},persistedIdentityResumed:true,disconnected:!(await status()).connected};await writeFile(join(root,'.local/artifacts/product-ui-smoke.json'),JSON.stringify(result,null,2)+'\n');console.log('PRODUCT_UI_OK '+JSON.stringify(result));
  });
}finally{server.kill('SIGTERM');await pause(250);if(server.exitCode===null)server.kill('SIGKILL');await rm(folder,{recursive:true,force:true});}
