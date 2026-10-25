#!/usr/bin/env node
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {withBrowser,pngPixels,pause,root} from '../../../tools/browser.mjs';
await withBrowser({width:780,height:437,path:'/ui-test.html'},async({call,evaluate,waitFor})=>{
  await waitFor('document.documentElement?.dataset.ui',state=>state==='ready');
  const status=async()=>JSON.parse(await evaluate('window.__UI.ui_browser_status()'));
  const fence=async()=>{await evaluate('window.__UI.ui_browser_fence()');await evaluate('new Promise(requestAnimationFrame)');};
  const click=async(x,y)=>{await call('Input.dispatchMouseEvent',{type:'mouseMoved',x,y});await call('Input.dispatchMouseEvent',{type:'mousePressed',x,y,button:'left',clickCount:1});await call('Input.dispatchMouseEvent',{type:'mouseReleased',x,y,button:'left',clickCount:1});await fence();};
  const key=async(code,key)=>{await call('Input.dispatchKeyEvent',{type:'keyDown',code,key});await call('Input.dispatchKeyEvent',{type:'keyUp',code,key});await fence();};
  async function capture(name){await fence();const url=await evaluate("document.querySelector('canvas').toDataURL('image/png')");const png=Buffer.from(url.split(',')[1],'base64');const image=pngPixels(png);assert.equal(image.width,780);assert.equal(image.height,437);let light=0;for(let y=0;y<437;y++)for(let x=0;x<780;x++){const pixel=image.sample(x,y);if(pixel[0]>200&&pixel[1]>190&&pixel[2]>170)light++;}assert(light>1000,`UI ${name} missing actual pixel font`);await mkdir(join(root,'.local/artifacts'),{recursive:true});await writeFile(join(root,'.local/artifacts',`m11-web-${name}.png`),png);return light;}
  assert.equal((await status()).screen,'Welcome');const welcomePixels=await capture('welcome');
  await click(295,292);assert.equal((await status()).screen,'Settings(Graphics)');await click(475,78);assert.equal((await status()).screen,'Settings(Sound)');await click(700,123);assert.equal((await status()).masterVolume,0);const settingsPixels=await capture('settings');
  await evaluate('window.__UI.ui_browser_fixture_inventory()');await fence();assert.equal((await status()).screen,'Inventory');const before=(await status()).filteredItems;assert(before>=180);const inventoryPixels=await capture('inventory');
  // Real browser keyboard and pointer events enter the same Rust state machine.
  await click(570,90);for(const [code,char]of[['KeyB','ж'],['KeyT','е'],['KeyK','л']])await key(code,char);assert((await status()).filteredItems>0);assert((await status()).filteredItems<before);await key('Escape','Escape');assert.equal((await status()).screen,'Game');
  await key('F3','F3');assert.equal((await status()).debug,true);const profilerPixels=await capture('profiler');
  const result={welcomePixels,settingsPixels,inventoryPixels,profilerPixels,itemCount:before,state:await status(),actualWebGPUCanvas:true,actualPointerAndKeyboard:true};await writeFile(join(root,'.local/artifacts','m11-ui-browser-smoke.json'),JSON.stringify(result,null,2)+'\n');console.log(`UI_BROWSER_OK ${JSON.stringify(result)}`);
});
