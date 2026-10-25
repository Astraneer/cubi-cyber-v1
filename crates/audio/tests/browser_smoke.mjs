#!/usr/bin/env node
import assert from 'node:assert/strict';
import {mkdir,writeFile,rm} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {join} from 'node:path';
import {withBrowser,pause,root} from '../../../tools/browser.mjs';
const artifacts=join(root,'.local/artifacts');await mkdir(artifacts,{recursive:true});
const fixture=join(root,'.local/tmp',`audio-privacy-${process.pid}.zip`),minecraft=join(root,'.local/tmp',`audio-minecraft-${process.pid}`,'.minecraft'),canary=`PRIVATE_LOCAL_SOUND_${process.pid}_NEVER_UPLOAD`;
const python=spawnSync('python3',['-c',`import sys,zipfile,hashlib,json,pathlib
with zipfile.ZipFile(sys.argv[1],'w',compression=zipfile.ZIP_STORED) as z:
 z.writestr('pack.mcmeta','{"pack":{"pack_format":34,"description":"'+sys.argv[2]+'"}}')
 z.writestr('assets/privacy/sounds/private_canary.ogg',sys.argv[2]*64)
 z.writestr('assets/privacy/textures/block/stone_n.png',sys.argv[2]*4)
 z.writestr('CREDITS.md','Local-only private owner notice')
sound=pathlib.Path('donor/web/sounds/ui/click_1.ogg').read_bytes();digest=hashlib.sha1(sound).hexdigest();base=pathlib.Path(sys.argv[3]);(base/'assets/indexes').mkdir(parents=True);(base/'assets/objects'/digest[:2]).mkdir(parents=True);(base/'assets/objects'/digest[:2]/digest).write_bytes(sound)
(base/'assets/indexes/test.json').write_text(json.dumps({'objects':{'minecraft/sounds/random/click.ogg':{'hash':digest,'size':len(sound)}}}))
(base/'launcher_accounts.json').write_text(sys.argv[2]+'PRIVATE_ACCOUNT_NEVER_READ')
`,fixture,canary,minecraft],{cwd:root,encoding:'utf8'});assert.equal(python.status,0,python.stderr);
try {await withBrowser({width:640,height:320,path:'/audio-test.html'},async({call,evaluate,waitFor,onEvent})=>{
  await waitFor('document.documentElement?.dataset.audio',v=>v==='ready');await pause(200);
  const oracle=JSON.parse(await evaluate('window.__AUDIO.audio_offline_oracle()'));
  assert(oracle.panMaxError<=1e-5);assert(oracle.caveTailEnergy>0.0001);assert.equal(oracle.plainTailEnergy,0);assert(oracle.firstEchoFrame>=1390&&oracle.firstEchoFrame<1410);assert(oracle.defaultDecodedFrames>100);
  await call('Network.enable');let requests=[];const offNetwork=onEvent('Network.requestWillBeSent',event=>requests.push({url:event.request.url,body:event.request.postData??''}));
  await call('Page.setInterceptFileChooserDialog',{enabled:true});let opened;const offChooser=onEvent('Page.fileChooserOpened',value=>{opened=value;});
  await call('Input.dispatchMouseEvent',{type:'mousePressed',x:65,y:20,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:65,y:20,button:'left',clickCount:1});
  const end=Date.now()+10000;while(!opened&&Date.now()<end)await pause(20);assert(opened,'Actual file picker did not open');
  await call('DOM.setFileInputFiles',{files:[fixture],backendNodeId:opened.backendNodeId});
  const imported=JSON.parse(await waitFor('window.__AUDIO.local_resource_summary()',s=>JSON.parse(s).files===4));assert.equal(imported.sounds,1);assert.equal(imported.textures,1);assert.equal(imported.notices,1);
  const status=await evaluate("document.getElementById('status').textContent");assert(status.includes('локально'));assert(!status.includes(canary));
  opened=undefined;await call('Input.dispatchMouseEvent',{type:'mousePressed',x:210,y:20,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:210,y:20,button:'left',clickCount:1});
  const directoryEnd=Date.now()+10000;while(!opened&&Date.now()<directoryEnd)await pause(20);assert(opened,'Actual Minecraft directory picker did not open');await call('DOM.setFileInputFiles',{files:[minecraft],backendNodeId:opened.backendNodeId});
  const minecraftSummary=JSON.parse(await waitFor('window.__AUDIO.local_resource_summary()',s=>JSON.parse(s).revision===2));assert.equal(minecraftSummary.files,2,'Accounts/non-sound objects entered local asset store');assert.equal(minecraftSummary.sounds,1);
  await pause(500);assert.equal(requests.length,0,`Import attempted network I/O: ${JSON.stringify(requests)}`);assert(!requests.some(v=>v.url.includes(canary)||v.body.includes(canary)));offChooser();offNetwork();
  const complete=new Promise(resolve=>{const off=onEvent('Tracing.tracingComplete',result=>{off();resolve(result);});});
  await call('Tracing.start',{transferMode:'ReturnAsStream',categories:'audio,webaudio,disabled-by-default-audio,blink.user_timing,toplevel',options:'record-as-much-as-possible,enable-sampling'});
  const live=JSON.parse(await evaluate('window.__AUDIO.start_audio_benchmark(false,64)'));assert(live.running);assert.equal(live.voices,64);await pause(300);
  await evaluate("performance.mark('m12-plain-start')");await pause(1000);await evaluate("performance.mark('m12-plain-end')");
  await evaluate('window.__AUDIO.set_audio_cave(true)');await pause(300);await evaluate("performance.mark('m12-cave-start')");await pause(1000);await evaluate("performance.mark('m12-cave-end')");
  await call('Tracing.end');const traceEnd=await Promise.race([complete,pause(15000).then(()=>{throw new Error('Audio trace did not finish');})]);let raw='';
  while(raw.length<32*1024*1024){const chunk=await call('IO.read',{handle:traceEnd.stream,size:65536});raw+=chunk.base64Encoded?Buffer.from(chunk.data,'base64').toString('utf8'):chunk.data;if(chunk.eof)break;}
  await call('IO.close',{handle:traceEnd.stream});await evaluate('window.__AUDIO.stop_audio_benchmark()');assert(raw.length<32*1024*1024,'Audio trace exceeded limit');
  const trace=JSON.parse(raw).traceEvents,marks=Object.fromEntries(trace.filter(e=>e.name?.startsWith('m12-')).map(e=>[e.name,e.ts]));
  const render=trace.filter(e=>/render|process|convolv/i.test(e.name??'')&&/audio/i.test(`${e.cat} ${e.name}`));
  const names=[...new Set(render.map(e=>e.name))];
  function stage(name){
    const samples=render.filter(e=>e.name==='RealtimeAudioDestinationHandler::Render'&&e.args?.frames===128&&e.ts>=marks[`m12-${name}-start`]&&e.ts<=marks[`m12-${name}-end`]&&e.ph==='X'&&Number.isFinite(e.dur));
    const cpu=samples.filter(e=>Number.isFinite(e.tdur)),values=cpu.map(e=>e.tdur/1000).sort((a,b)=>a-b);
    const mean=values.length?values.reduce((a,b)=>a+b,0)/values.length:null;
    return{quantumFrames:128,events:samples.length,cpuDurationEvents:cpu.length,cpuMeanMs:mean,cpuP95Ms:values.length?values[Math.floor(values.length*.95)]:null,cpuP99Ms:values.length?values[Math.floor(values.length*.99)]:null,cpuMaxMs:values.length?values.at(-1):null,wallMaxMs:samples.length?Math.max(...samples.map(e=>e.dur))/1000:null};
  }
  const timing={plain:stage('plain'),cave:stage('cave'),eventNames:names,live,threadCpuMeasured:false};
  // Only a real thread-duration sample can support a CPU claim. Raw callback
  // wall time and offline processing speed remain separately named evidence.
  timing.threadCpuMeasured=timing.cave.cpuDurationEvents>0;
  timing.echoMeanDeltaMs=timing.threadCpuMeasured?timing.cave.cpuMeanMs-timing.plain.cpuMeanMs:null;
  timing.isolatedConvolverCpuMeasured=false;
  timing.h8Accepted=false;
  await writeFile(join(artifacts,'audio-cpu-trace.json'),raw);
  const result={oracle,privacy:{files:imported.files,bytes:imported.bytes,minecraft:minecraftSummary,outgoingRequests:requests.length,actualFilePicker:true,actualDirectoryPicker:true,accountFilesExcluded:true},timing};await writeFile(join(artifacts,'m12-audio-smoke.json'),JSON.stringify(result,null,2)+'\n');
  console.log(`AUDIO_BROWSER_OK ${JSON.stringify(result)}`);
});} finally {await rm(fixture,{force:true});await rm(join(minecraft,'..'),{recursive:true,force:true});}
