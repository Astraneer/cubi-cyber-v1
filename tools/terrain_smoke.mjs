#!/usr/bin/env node
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { withBrowser, pngPixels, pause, root } from './browser.mjs';

const headed=process.argv.includes('--headed');
await withBrowser({headed,width:320,height:224,path:'/?scene=forest'},async({call,evaluate,waitFor})=>{
  const result=await waitFor(`JSON.stringify({state:document.documentElement.dataset.m2,result:window.__M2})`,text=>{
    const value=JSON.parse(text);if(value.state==='error')throw new Error(JSON.stringify(value));
    return value.state==='ready'&&value.result?.ready;
  });
  const initial=JSON.parse(result).result;
  assert.equal(initial.residentPages,27);assert.equal(initial.loadedPages,125);
  assert(initial.residentQuads>=500,'world is empty or generation/meshing did not execute');
  assert(initial.terrainFrames>=1);assert.equal(initial.sampleCount,4);
  assert.equal(initial.framePipelineCompilations,0);
  const png=await evaluate(`document.querySelector('canvas').toDataURL('image/png')`);
  assert(png.startsWith('data:image/png;base64,'));
  const encoded=Buffer.from(png.split(',')[1],'base64'), image=pngPixels(encoded);
  assert.equal(image.width,320);assert.equal(image.height,224);
  let green=0,opaque=0,samples=0;const colors=new Set();
  for(let y=0;y<image.height;y+=2)for(let x=0;x<image.width;x+=2){
    const p=image.sample(x,y);samples++;if(p[3]===255)opaque++;
    if(p[1]>p[0]*1.1&&p[1]>p[2]*1.1&&p[1]>20)green++;
    colors.add((p[0]>>3)+32*(p[1]>>3)+1024*(p[2]>>3));
  }
  assert(opaque>samples*0.99,'canvas is transparent');
  assert(green>samples*0.03&&colors.size>32,'canvas lacks textured forest geometry');
  const artifacts=join(root,'.local/artifacts');await mkdir(artifacts,{recursive:true});
  await writeFile(join(artifacts,`m2-world-${headed?'headed':'headless'}.png`),encoded);
  if(headed){
    const shot=await call('Page.captureScreenshot',{format:'png',fromSurface:true});
    const screen=pngPixels(Buffer.from(shot.data,'base64'));
    assert(screen.sample(160,100)[3]===255,'headed compositor omitted canvas');
    await writeFile(join(artifacts,'m2-world-compositor.png'),Buffer.from(shot.data,'base64'));
  }
  // Input must activate the shared actor and actually move it through loaded cells.
  await call('Input.dispatchKeyEvent',{type:'keyDown',code:'KeyW',key:'w'});
  await pause(100);const before=JSON.parse(await evaluate('JSON.stringify(window.__M2Stats())'));
  await pause(600);await call('Input.dispatchKeyEvent',{type:'keyUp',code:'KeyW',key:'w'});
  const after=JSON.parse(await evaluate('JSON.stringify(window.__M2Stats())'));
  assert(after.playerSpawned,'input did not activate player');
  const distance=Math.hypot(after.playerPosition[0]-before.playerPosition[0],after.playerPosition[2]-before.playerPosition[2]);
  assert(distance>0.2,'WASD did not move the player');
  assert.equal(after.framePipelineCompilations,0);
  await writeFile(join(artifacts,'m2-world-smoke.json'),JSON.stringify({initial,after,image:{green,colors:colors.size,samples},distance},null,2)+'\n');
  console.log(`TERRAIN_BROWSER_OK ${JSON.stringify({pages:initial.residentPages,quads:initial.residentQuads,loaded:initial.loadedPages,green,colors:colors.size,movement:distance,headed})}`);
});
