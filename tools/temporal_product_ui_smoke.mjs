// Candidate-only UI2 proof. Graphics/gameplay use physical input only.
// The fresh fixture Owner credential is installed through normal DOMStorage,
// as an existing authenticated browser profile; no World/settings state is set.
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {mkdtemp,mkdir,readFile,writeFile} from 'node:fs/promises';
import {join,resolve,relative} from 'node:path';
import {createHash} from 'node:crypto';
import {withBrowser,root,pause} from './browser.mjs';
import {componentProfile} from './ui_graphics_controls.mjs';
import {fixtureOwnerCredential} from './fixture_owner_auth.mjs';
import {UI_PRESENTATION_SNAPSHOT,waitForUiPresentation} from './ui_presentation_barrier.mjs';

assert(process.env.CUBI_TAA_PAIR,'a copied coherent candidate pair is required');
const pair=resolve(process.env.CUBI_TAA_PAIR);assert(relative(root,pair).startsWith('.local/artifacts/'));
const manifest=JSON.parse(await readFile(join(pair,'manifest.json'),'utf8'));
const candidateUi=JSON.parse(await readFile(join(root,'.local/artifacts/taa-mode2-candidate-ui.json'),'utf8'));
assert.equal(manifest.noCompilerInputMutationDuringBuild,true);
for(const name of ['native','replay','wasm'])assert.equal(manifest.buildResults?.[name],0);
assert(manifest.snapshotSourceFilesChecked>=1600);
const snapshot=resolve(manifest.snapshot);assert(relative(root,snapshot).startsWith('.local/checkpoints/'));
const sha=async path=>createHash('sha256').update(await readFile(path)).digest('hex');
for(const name of ['server','replay'])assert.equal(await sha(join(pair,name)),manifest.binaries?.[name]?.sha256);
assert.equal(await sha(join(pair,'web-dist/pkg/client_web_bg.wasm')),manifest.wasm?.sha256);
for(const file of ['crates/net/src/codec.rs','crates/client/src/view/temporal.rs','crates/render/src/recipe_board_motion.rs','crates/ui/src/settings.rs','crates/client-web/src/lib.rs'])assert.equal(await sha(join(snapshot,file)),manifest.inputs?.[file]);
for(const row of candidateUi.files.filter(row=>!row.path.includes('/tests/')))assert.equal(manifest.inputs?.[row.path],row.after_sha256,`isolated UI2 candidate required: ${row.path}`);
process.env.CUBI_PRODUCT_SERVER=join(pair,'server');process.env.CUBI_PRODUCT_REPLAY=join(pair,'replay');process.env.CUBI_WEB_DIST=join(pair,'web-dist');process.env.CUBI_WEB_SERVER_SCRIPT=join(snapshot,'tools/serve_web.py');
assert(!process.env.CUBI_WEBGPU_TRACE&&!process.env.CUBI_WEBGPU_SEGMENTS,'candidate proof uses the ordinary completion path');
const folder=await mkdtemp(join(root,'.local/tmp/temporal-product-'));
const artifacts=resolve(process.env.CUBI_TAA_ARTIFACTS??join(root,'.local/artifacts/temporal-product-browser'));
await mkdir(artifacts,{recursive:true});const downloads=join(folder,'downloads');await mkdir(downloads);
const ready=join(folder,'ready');let serverLog='';
const server=spawn(process.env.CUBI_PRODUCT_SERVER,['--data',join(folder,'world'),'--ws','127.0.0.1:0','--udp','127.0.0.1:0','--ready-file',ready],{cwd:root,stdio:['ignore','pipe','pipe']});
server.stdout.on('data',b=>serverLog+=b);server.stderr.on('data',b=>serverLog+=b);
try{
  let address;for(let i=0;i<200;i++){try{address=(await readFile(ready,'utf8')).match(/ws=(\S+)/)?.[1];}catch{}if(address)break;if(server.exitCode!==null)throw Error(serverLog);await pause(100);}assert(address,serverLog);address=address.includes('://')?address:`ws://${address}`;
  const encodedOwner=await readFile(join(folder,'world/owner.identity'));let owner;
  try{owner=fixtureOwnerCredential(encodedOwner);}finally{encodedOwner.fill(0);}
  await withBrowser({path:'/',width:780,height:437},async({call,evaluate,waitFor,url})=>{
    await waitFor('document.documentElement?.dataset.app',v=>v==='ready');
    const ui=()=>evaluate('window.__CUBI.product_ui_stats()'),stats=()=>evaluate('window.__M2Stats()');
    const waitUi=(predicate,timeout=120000)=>waitFor('window.__CUBI.product_ui_stats()',predicate,timeout);
    const painted=()=>waitForUiPresentation({read:()=>evaluate(UI_PRESENTATION_SNAPSHOT),pause});
    const key=async(code,key=code)=>{await call('Input.dispatchKeyEvent',{type:'keyDown',code,key});await call('Input.dispatchKeyEvent',{type:'keyUp',code,key});};
    const type=async(text)=>{for(const char of text)await key(char>='0'&&char<='9'?`Digit${char}`:char==='.'?'Period':char===':'?'Semicolon':'KeyX',char);};
    const click=async(x,y,button='left')=>{await painted();await call('Input.dispatchMouseEvent',{type:'mouseMoved',x,y});await call('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button,clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x,y,button,clickCount:1});await pause(150);};
    async function widget(name,button='left'){
      await painted();const box=await evaluate(`window.__CUBI.product_ui_widget(${JSON.stringify(name)})`);assert(box,`visible actual control ${name}`);
      await click(box[0]+box[2]/2,box[1]+box[3]/2,button);
    }
    const identity=()=>evaluate("Object.keys(localStorage).filter(k=>k.startsWith('cubi.identity.')).map(k=>localStorage.getItem(k).split(':')[0]).sort().join(',')");
    async function savedMode(expected){
      const mode=await evaluate(`(()=>{const h=localStorage.getItem('cubi.ui.preferences')??'';if(!/^(?:[0-9a-f]{2})+$/i.test(h))return null;const b=Uint8Array.from(h.match(/../g),v=>parseInt(v,16));if(b.length<141||String.fromCharCode(...b.subarray(0,4))!=='UIP1')return null;const d=new DataView(b.buffer),n=d.getUint16(4,true);if(n!==135||b.length<6+n||String.fromCharCode(...b.subarray(6,10))!=='CUI1'||d.getUint16(11,true)!==32)return null;return d.getInt32(25,true);})()`);
      assert.equal(mode,expected,'actual CUI1 saved antialiasing value');
    }
    async function game(mode,deadline=Date.now()+120000){
      assert(Date.now()<deadline,'Game120Deadline');
      const observed=await waitFor('({ui:window.__CUBI.product_ui_stats(),stats:window.__M2Stats()})',value=>{
        const u=value?.ui,s=value?.stats;
        return u?.screen==='Game'&&u.requestedSamples===mode&&(u.settingsApplied&8)!==0&&s?.connected&&s.playerSpawned&&s.lightReady&&s.gameInputAdmitted===true&&s.gameUiPresentationPending===false&&s.sampleCount===(mode===4?4:1);
      },Math.max(0,deadline-Date.now()));
      // Admission already follows the actual first-world and cached Game-HUD
      // GPU completions. A newer unrelated HDR is not another startup barrier.
      assert(Date.now()<=deadline,'Game120Deadline');
      assert.equal(observed.ui.smaaPrepared,0);await savedMode(mode);
      assert(Date.now()<=deadline,'Game120Deadline');return observed.stats;
    }
    async function menu(){await key('Escape','Escape');await waitUi(s=>s.screen==='Pause',15000);await widget('Settings');await waitUi(s=>s.screen==='Settings(Graphics)',15000);}
    delete process.env.CUBI_UI_COMPONENT_DAY;delete process.env.CUBI_UI_COMPONENT_SMAA;
    const manual=await componentProfile({call,evaluate,painted,click,ui});
    await widget('Settings');await click(130,78);
    for(let n=0;n<3&&(await ui()).requestedSamples!==2;n++)await widget('Setting(Msaa)');
    assert.equal((await ui()).requestedSamples,2,'candidate UI2 must be physically available');
    let daySelected=false;
    for(let attempt=0;attempt<32;attempt++){
      await painted();const box=await evaluate("window.__CUBI.product_ui_widget('Setting(ViewTime)')");
      if(box&&box[1]+box[3]/2>110&&box[1]+box[3]/2<355){
        assert.equal((await ui()).viewTimeChoice,0);
        await click(box[0]+box[2]/2,box[1]+box[3]/2);
        assert.equal((await ui()).viewTimeChoice,1);daySelected=true;break;
      }
      await call('Input.dispatchMouseEvent',{type:'mouseWheel',x:400,y:220,deltaX:0,deltaY:80});
    }
    assert(daySelected,'actual Welcome personal DAY control required');manual.saved.viewTimeChoice=1;
    await widget('Back');
    await call('DOMStorage.enable');
    const ownerName=owner.name;
    try{await call('DOMStorage.setDOMStorageItem',{storageId:{securityOrigin:new URL(url).origin,isLocalStorage:true},key:`cubi.identity.${new URL(address).href}`,value:owner.storageValue});}
    finally{owner.storageValue='';owner=undefined;}
    await widget('Name');for(let n=0;n<80;n++)await key('Backspace','Backspace');await type(ownerName);
    await widget('Address');for(let n=0;n<80;n++)await key('Backspace','Backspace');await type(address);
    const firstDeadline=Date.now()+120000;await click(350,250);
    const first=await game(2,firstDeadline),firstIdentity=await identity();assert(firstIdentity);await waitUi(s=>s.role==='Owner',15000);
    await waitFor('window.__M2Stats()',s=>s.timeOfDay===6000&&s.sunY>.999,15000);
    assert.equal((await ui()).viewTimeChoice,1);assert((await ui()).settingsApplied&(1<<29));
    await key('KeyT','t');await type('/give 3 7');await key('Enter','Enter');await waitUi(s=>s.inventoryItems===7,15000);
    const stages=[{mode:2,inventory:7,receipt:first}];
    for(const [mode,button] of [[4,'right'],[2,'left']]){
      await menu();const deadline=Date.now()+120000;await widget('Setting(Msaa)',button);const receipt=await game(mode,deadline);
      await waitUi(s=>s.inventoryItems===7,15000);assert.equal(await identity(),firstIdentity);stages.push({mode,inventory:7,receipt});
    }
    const old=await evaluate('({width:document.querySelector("canvas").width,height:document.querySelector("canvas").height})');
    await call('Emulation.setDeviceMetricsOverride',{width:3440,height:1440,deviceScaleFactor:1,mobile:false});
    await waitFor('window.__M2Stats()',s=>s.uiStatus.includes('МиБ'),15000);await painted();
    const rejected=await stats(),retained=await evaluate('({width:document.querySelector("canvas").width,height:document.querySelector("canvas").height})');
    assert.deepEqual(retained,old);assert(rejected.connected);assert.equal(rejected.sampleCount,1);assert.equal((await ui()).requestedSamples,2);assert.equal((await ui()).inventoryItems,7);assert.equal(await identity(),firstIdentity);
    const png=await evaluate("document.querySelector('canvas').toDataURL('image/png')");await writeFile(join(artifacts,'oversize-rejected.png'),Buffer.from(png.split(',')[1],'base64'));
    await call('Emulation.setDeviceMetricsOverride',{width:780,height:437,deviceScaleFactor:1,mobile:false});
    await painted();await widget('Back');await key('Escape','Escape');await game(2);
    await key('Escape','Escape');await waitUi(s=>s.screen==='Pause',15000);await widget('Disconnect');await waitUi(s=>s.screen==='Welcome',15000);
    const resumeDeadline=Date.now()+120000;await click(350,250);const resumed=await game(2,resumeDeadline);await waitUi(s=>s.inventoryItems===7,15000);assert.equal(await identity(),firstIdentity);
    await call('Input.dispatchKeyEvent',{type:'keyDown',code:'KeyW',key:'w'});await pause(250);await call('Input.dispatchKeyEvent',{type:'keyUp',code:'KeyW',key:'w'});await pause(300);
    const movement=await waitFor('window.__M2Stats()',s=>s.terrainFrames>resumed.terrainFrames&&s.currentCamera===s.renderedCamera,15000);await painted();
    await call('Browser.setDownloadBehavior',{behavior:'allow',downloadPath:downloads,eventsEnabled:true});await key('F8','F8');
    let report;for(let n=0;n<150;n++){try{report=JSON.parse(await readFile(join(downloads,'cubi-flight-recorder.json'),'utf8'));break;}catch{}await pause(100);}assert(report?.frames.some(f=>f.frame>=resumed.submittedFrames&&f.gpuTotalComplete&&f.gpuNs>0),'post-reconnect mode2 actual complete GPU frame required');assert(report.movementReplayAvailable&&report.movementReplay.steps>0);
    const flight=join(artifacts,'flight.json');await writeFile(flight,JSON.stringify(report));const replay=spawnSync(process.env.CUBI_PRODUCT_REPLAY,[flight],{cwd:root,encoding:'utf8'});assert.equal(replay.status,0,replay.stderr);assert(replay.stdout.includes('MOVEMENT_REPLAY_OK'));
    const result={scope:'candidate-only actual temporal product shell; manual DAY/optics0 component',fullProfileAccepted:false,mode2ExposedInShipping:false,coherentPair:{head:manifest.head,noMutation:true,wasmSha256:manifest.wasm.sha256},ownFixtureAuthBootstrap:true,manual,stages,oversize:{requested:[3440,1440],retained,notice:rejected.uiStatus},sameIdentity:true,inventoryRetained:7,reconnect:resumed,movement:{terrainFramesBefore:resumed.terrainFrames,terrainFramesAfter:movement.terrainFrames,cameraCommitted:true},f8:{frames:report.frames.length,completeGpu:true,movementSteps:report.movementReplay.steps,offlineReplay:true}};
    await writeFile(join(artifacts,'proof.json'),JSON.stringify(result,null,2)+'\n');console.log('TEMPORAL_PRODUCT_BROWSER_OK '+JSON.stringify(result));
  });
}finally{
  server.kill('SIGTERM');await writeFile(join(artifacts,'server.log'),serverLog);
}
