#!/usr/bin/env node
// Real browser controls and an equal-camera, GPU-fenced H1 image oracle.
// The publication barrier tests correctness; it does not measure GPU latency.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { withBrowser, pngPixels, pause, root } from './browser.mjs';
const run=promisify(execFile), headed=process.argv.includes('--headed');
const dir=join(root,'.local/artifacts',`m3-${Date.now()}`);await mkdir(dir,{recursive:true});
await withBrowser({headed,width:780,height:437,path:'/?scene=forest&meshDelayMs=200&workerMeshDelayMs=200'},async({call,evaluate,waitFor})=>{
  await waitFor('document.documentElement.dataset.m2',s=>{if(s==='error')throw new Error('Client loading failed');return s==='ready';});
  const stats=async()=>JSON.parse(await evaluate('JSON.stringify(window.__M2Stats())'));
  async function until(test,timeout=20000){const end=Date.now()+timeout;let last;while(Date.now()<end){last=await stats();if(test(last))return last;await pause(30);}throw new Error(`Player condition timed out: ${JSON.stringify(last)}`);}
  const tap=async(code,key)=>{await call('Input.dispatchKeyEvent',{type:'keyDown',code,key});await call('Input.dispatchKeyEvent',{type:'keyUp',code,key});};
  const fence=async(test=()=>true)=>until(s=>s.submittedFrames===s.completedFrames&&s.inFlightFrames===0&&s.currentCamera===s.renderedCamera&&s.renderedUploadBytes===s.uploadBytes&&test(s));
  const capture=async(name)=>{const data=await evaluate("document.querySelector('canvas').toDataURL('image/png')");assert(data.startsWith('data:image/png;base64,'));const bytes=Buffer.from(data.split(',')[1],'base64');const image=pngPixels(bytes);assert.equal(image.width,780);assert.equal(image.height,437);const path=join(dir,`${name}.png`);await writeFile(path,bytes);return path;};
  const compare=async(reference,actual,control)=>{const {stdout}=await run('cargo',['run','--locked','-p','render','--example','compare_frames','--',reference,actual,...(control?[control]:[])],{cwd:root,timeout:120000,maxBuffer:1000000});const metric=JSON.parse(stdout.trim());assert(metric.blackControlFlipMean>0.05,'FLIP positive control failed');return metric;};
  const checkTransient=metric=>{assert(metric.flipMean<=0.001,`H1 transient FLIP: ${JSON.stringify(metric)}`);assert(metric.maxDelta<=4,`H1 transient edge/color: ${JSON.stringify(metric)}`);};
  const initial=await stats();assert.equal(initial.residentPages,27);assert.equal(initial.loadedPages,125);assert(initial.residentQuads>=500);assert.equal(initial.sampleCount,4);
  await tap('KeyV','v');const avatar=await until(s=>s.playerSpawned&&s.cameraMode===1&&s.selfModelRendered);await capture('third-person');
  await tap('KeyH','h');await pause(100);const denied=await stats();assert.equal(denied.deniedDebugRequests,1);assert.equal(denied.noclip,false);
  await tap('KeyV','v');await tap('KeyF','f');const height=(await stats()).playerPosition[1];
  await call('Input.dispatchKeyEvent',{type:'keyDown',code:'Space',key:' '});await pause(400);await call('Input.dispatchKeyEvent',{type:'keyUp',code:'Space',key:' '});
  const flight=await stats();assert(flight.flying&&flight.playerPosition[1]>height+0.5);
  await tap('KeyF','f');await until(s=>s.onGround&&!s.flying);await until(s=>s.fullMeshReady);
  await call('Input.dispatchMouseEvent',{type:'mousePressed',x:390,y:220,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:390,y:220,button:'left',clickCount:1});await pause(100);
  assert(await evaluate('!!document.pointerLockElement'),'Pointer lock missing after trusted click');
  await evaluate("document.dispatchEvent(new MouseEvent('mousemove',{movementX:0,movementY:220,bubbles:true}))");
  await pause(2000);await fence(s=>s.fullMeshReady&&s.queuedJobs===0&&s.runningJobs===0&&s.residentPages===27&&s.loadedPages===125);assert((await stats()).targetPosition);
  const before=await capture('before-edit');
  await evaluate("import('./pkg/client_web.js').then(m=>m.pause_demo_mesh_publication(true))");
  await call('Input.dispatchMouseEvent',{type:'mousePressed',x:390,y:220,button:'left',clickCount:1});await until(s=>s.blocksBroken===1,10000);await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:390,y:220,button:'left',clickCount:1});
  const fast=await fence(s=>s.editOverlayUploads>0&&!s.fullMeshReady);const fastImage=await capture('break-fast');assert.equal(fast.demoMeshDelayTicks,4);
  await evaluate("import('./pkg/client_web.js').then(m=>m.pause_demo_mesh_publication(false))");const final=await fence(s=>s.fullMeshReady);const finalImage=await capture('break-final');
  assert.equal(final.currentCamera,fast.currentCamera);assert.equal(final.worldVersion,fast.worldVersion);
  const breaking=await compare(finalImage,fastImage,before);checkTransient(breaking);assert(breaking.negativeControl.maxDelta>4&&breaking.negativeControl.flipMean>Math.max(breaking.flipMean*4,0.000001),'Old-base negative control failed');
  assert(await evaluate("import('./pkg/client_web.js').then(m=>m.rollback_demo_edit())"));await fence(s=>s.fullMeshReady&&s.editsRolledBack===1);const restored=await capture('restored');const rollback=await compare(before,restored);assert.equal(rollback.differingPixels,0);
  await evaluate("import('./pkg/client_web.js').then(m=>m.pause_demo_mesh_publication(true))");
  await call('Input.dispatchMouseEvent',{type:'mousePressed',x:390,y:220,button:'right',clickCount:1});await until(s=>s.blocksPlaced>0);await call('Input.dispatchMouseEvent',{type:'mouseReleased',x:390,y:220,button:'right',clickCount:1});const placeFast=await fence(s=>s.editShellPages>0);const placeFastImage=await capture('place-fast');
  await evaluate("import('./pkg/client_web.js').then(m=>m.pause_demo_mesh_publication(false))");const placeFinal=await fence(s=>s.fullMeshReady);assert.equal(placeFast.currentCamera,placeFinal.currentCamera);assert.equal(placeFast.worldVersion,placeFinal.worldVersion);const placing=await compare(await capture('place-final'),placeFastImage,restored);checkTransient(placing);
  // A separate edit runs without the publication barrier. Count actual completed
  // GPU/presentation callbacks carrying its revision, not CPU encode attempts.
  await evaluate("import('./pkg/client_web.js').then(m=>m.reset_edit_presentation_probe())");
  assert(await evaluate("import('./pkg/client_web.js').then(m=>m.rollback_demo_edit())"));
  const presented=await fence(s=>s.presentationLatencyMeasured&&s.presentedEditCount>=1&&s.pendingEditPresentations===0);
  assert(presented.editPresentedFramesMax>=1&&presented.editPresentedFramesMax<=2,`Presented edit exceeded two completed frames: ${JSON.stringify(presented)}`);
  assert.equal(presented.presentationProbeOverflow,false);
  const result={initial,avatar,flight,denied,fast,final,breaking,rollback,placing,presented,presentationLatencyMeasured:true,workerDelayMs:200,visualPublicationBarrier:true};await writeFile(join(dir,'player-smoke.json'),JSON.stringify(result,null,2)+'\n');
  console.log(`PLAYER_BROWSER_OK ${JSON.stringify({breaking,rollback,placing,artifacts:dir,presentationLatencyMeasured:true,presentedEditFrames:presented.editPresentedFramesMax})}`);
});
