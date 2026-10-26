// Real input only: read-only widget bounds locate the control; CDP clicks it.
import assert from 'node:assert/strict';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {pngPixels,root,pause} from './browser.mjs';
export async function componentProfile({call,evaluate,painted,click,ui}) {
  await click(295,292);
  const choices={requestedHorizon:16,requestedSsao:0,requestedReflections:0,requestedClouds:0,requestedGi:0};
  for(let step=0;step<16&&(await ui()).requestedHorizon>16;step++){
    await painted();const box=await evaluate("window.__CUBI.product_ui_widget('Setting(Horizon)')");assert(box);
    await click(box[0]+box[2]/2,box[1]+box[3]/2,'right');
  }
  assert.equal((await ui()).requestedHorizon,16);
  for(const [name,field] of [['Ssao','requestedSsao'],['Reflections','requestedReflections'],['Clouds','requestedClouds'],['Gi','requestedGi']]) {
    let found=false;
    for(let attempt=0;attempt<32;attempt++) {
      await painted();
      const box=await evaluate(`window.__CUBI.product_ui_widget(${JSON.stringify(`Setting(${name})`)})`);
      if(box&&box[1]+box[3]/2>110&&box[1]+box[3]/2<355) {
        assert.equal((await ui())[field],1,`${name}: unexpected initial preference`);
        await click(box[0]+box[2]/2,box[1]+box[3]/2,'right');
        assert.equal((await ui())[field],0,`${name}: actual right click did not save0`);
        found=true;break;
      }
      await call('Input.dispatchMouseEvent',{type:'mouseWheel',x:400,y:220,deltaX:0,deltaY:80});
    }
    assert(found,`component settings control missing: ${name}`);
  }
  await painted();
  const box=await evaluate("window.__CUBI.product_ui_widget('Back')");assert(box);
  await click(box[0]+box[2]/2,box[1]+box[3]/2);
  return {actualWelcomeButtons:true,saved:choices};
}
export async function graphicsControls({call,evaluate,waitFor,waitUi,painted,key,click,status,ui,requireFar=true}) {
  async function widget(name,scroll=false) {
    for(let attempt=0;attempt<32;attempt++) {
      await painted();
      const box=await evaluate(`window.__CUBI.product_ui_widget(${JSON.stringify(name)})`);
      if(box&&(!scroll||(box[1]+box[3]/2>110&&box[1]+box[3]/2<355))) { await click(box[0]+box[2]/2,box[1]+box[3]/2); return; }
      if(!scroll){await pause(100);continue;}
      await call('Input.dispatchMouseEvent',{type:'mouseWheel',x:400,y:220,deltaX:0,deltaY:80});
    }
    assert.fail(`scrolled control missing: ${name}`);
  }
  async function menu(name){
    await key('Escape','Escape');await waitUi(s=>s.screen==='Pause');await widget(name);
    await waitUi(s=>s.screen===(name==='Settings'?'Settings(Graphics)':name));
  }
  async function world(name) {
    await waitUi(s=>s.screen==='Game');await painted();
    const url=await evaluate("document.querySelector('canvas').toDataURL('image/png')");
    const bytes=Buffer.from(url.split(',')[1],'base64');
    await writeFile(join(root,'.local/artifacts',`product-${name}.png`),bytes);
    return pngPixels(bytes);
  }
  function compare(a,b) {
    let changed=0,daySum=0,nightSum=0;
    for(let y=10;y<200;y++)for(let x=10;x<770;x++) {
      const p=a.sample(x,y),q=b.sample(x,y);
      if(Math.max(...p.slice(0,3).map((v,i)=>Math.abs(v-q[i])))>8)changed++;
      daySum+=p[0]+p[1]+p[2];nightSum+=q[0]+q[1]+q[2];
    }
    return {changed,meanA:daySum/(190*760*3),meanB:nightSum/(190*760*3)};
  }
  const identity=await evaluate("Object.keys(localStorage).filter(key=>key.startsWith('cubi.identity.')).map(key=>localStorage.getItem(key).split(':')[0]).join(',')");
  const initial=await status();
  await menu('Crossroads');await widget('Day');
  await waitFor('window.__M2Stats()',s=>s.sunClockKnown&&s.timeOfDay===6000&&s.sunY>.999,20000);
  await widget('Back');await key('Escape','Escape');const day=await world('personal-day');
  await menu('Crossroads');await widget('Night');
  const nightClock=await waitFor('window.__M2Stats()',s=>s.sunClockKnown&&s.timeOfDay===18000&&s.sunY<-.999,20000);
  await widget('Back');await key('Escape','Escape');const night=await world('personal-night');
  const solarPixels=compare(day,night);assert(solarPixels.changed>5000,`day/night physical image unchanged: ${JSON.stringify(solarPixels)}`);
  assert(nightClock.publicClockTick&&BigInt(nightClock.publicClockTick)>=BigInt(initial.publicClockTick),'personal override replaced public epoch');
  await writeFile(join(root,'.local/artifacts/product-graphics-partial.json'),JSON.stringify({fullProfileAccepted:false,completed:{personalTime:{actualButtons:true,dayTime:6000,nightTime:18000,publicEpochPreserved:true,...solarPixels}},remaining:['exposure pixels','MSAA reprepare']},null,2)+'\n');
  await menu('Crossroads');await widget('Day');await widget('Back');await key('Escape','Escape');
  const baseExposure=await world('exposure-before');const before=await status();
  await menu('Settings');
  for(let i=0;i<5;i++)await widget('Setting(Exposure)',true);
  const target=before.exposureGain+.25;
  const exposure=await waitFor('window.__M2Stats()',s=>Math.abs(s.exposureGain-target)<.001,20000);
  assert.equal((await ui()).settingsReloadRequired,0,'exposure unnecessarily requires resource reload');
  await widget('Back');await key('Escape','Escape');const exposed=await world('exposure-after');
  const exposurePixels=compare(baseExposure,exposed);assert(exposurePixels.changed>500,'actual tone compensation did not change pixels');
  assert.equal(exposure.framePipelineCompilations,0);
  await menu('Settings');await widget('Setting(Msaa)',true);
  await waitUi(s=>s.screen==='Game'&&s.requestedSamples===1,120000);
  const selected=await waitFor('window.__M2Stats()',s=>s.sampleCount===1&&s.connected&&s.lightReady&&(!requireFar||s.horizonHeightFront>0),120000);
  await world('msaa-one');
  const same=await evaluate(`Object.keys(localStorage).filter(key=>key.startsWith('cubi.identity.')).map(key=>localStorage.getItem(key).split(':')[0]).join(',')===${JSON.stringify(identity)}`);
  assert(same,'graphics reprepare replaced authority identity');
  assert.equal((await ui()).settingsReloadRequired,0);
  assert.equal(selected.framePipelineCompilations,0);
  await menu('Crossroads');await widget('ServerTime');
  await waitUi(s=>s.viewTimeChoice===0);await widget('Back');await key('Escape','Escape');
  const result={scope:'graphics controls',fullProfileAccepted:false,farHeightTiles:selected.horizonHeightFront,personalTime:{actualButtons:true,dayTime:6000,nightTime:18000,publicEpochPreserved:true,...solarPixels},exposure:{before:before.exposureGain,after:exposure.exposureGain,...exposurePixels},msaa:{before:initial.sampleCount,after:selected.sampleCount,asynchronousLoading:true,sameIdentity:true,noFramePipelineCompilation:true}};
  await writeFile(join(root,'.local/artifacts/product-graphics-controls.json'),JSON.stringify(result,null,2)+'\n');
  return result;
}
