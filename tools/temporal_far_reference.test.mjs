import test from 'node:test';
import assert from 'node:assert/strict';
import {analyzeTemporalFar,assertTemporalFar} from './temporal_far_reference.mjs';

function control(){
  const w=32,h=32,n=w*h;
  const front={committed:true,guard:true,heightFront:24,proxyFront:8,knownSources:8};
  const image=pillar=>Array.from({length:n*4},(_,i)=>i%4===3?255:pillar&&i/4%w>=6&&i/4%w<26&&Math.floor(i/4/w)>=6&&Math.floor(i/4/w)<26?200:100);
  const motion=stationary=>{
    const bytes=new Uint8Array(n*24),view=new DataView(bytes.buffer);
    for(let p=0;p<n;p++){
      view.setUint16(p*8,stationary?0:0x2800,true); // exact 1/32 UV = one pixel
      view.setUint16(p*8+6,0x3c00,true);
      view.setUint32(n*8+p*16,(p%w<16?16:17)<<24,true);
    }
    return [...bytes];
  };
  const results=['far-static','far-reference','far-pan','far-pan-reference','far-removed','far-removed-reference'].map(label=>({
    label,image:image(!label.startsWith('far-removed')),motion:motion(label==='far-static'),gpuTotalComplete:true,surfacePresented:true,
    presented:{samples:8,geometryEpoch:1,sourceEpoch:1},
  }));
  return {extent:[w,h],results,committed:front,replaced:front};
}
test('independent committed-source control passes',()=>assertTemporalFar(analyzeTemporalFar(control())));
test('camera-only motion that omits actual Proxy family is rejected',()=>{
  const value=control();for(const frame of value.results){const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer);for(let p=0;p<1024;p++)if(view.getUint32(8192+p*16,true)>>>24===17)view.setUint32(8192+p*16,0,true);frame.motion=[...bytes];}
  assert.throws(()=>assertTemporalFar(analyzeTemporalFar(value)),/Far family 17/);
});
test('trailing Far history is rejected without relaxing interior thresholds',()=>{
  const value=control();value.results.find(f=>f.label==='far-removed').image=value.results.find(f=>f.label==='far-pan-reference').image;
  assert.throws(()=>assertTemporalFar(analyzeTemporalFar(value)),/retains history/);
});
test('blanket source reset cannot substitute for actual camera motion',()=>{
  const value=control();value.results.find(f=>f.label==='far-pan').presented.sourceEpoch=2;
  assert.throws(()=>assertTemporalFar(analyzeTemporalFar(value)));
});

function cutoutControl(){
  const value=control();value.partialCutoutAlpha=128;
  for(const frame of value.results){
    if(frame.label.startsWith('far-removed'))continue;
    const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer);
    for(let y=4;y<16;y++)for(let x=8;x<24;x++){
      const p=x+32*y;frame.image.splice(p*4,3,32,180,40);
      for(let c=0;c<4;c++)view.setUint32(8192+p*16+c*4,0,true);
    }
    frame.motion=[...bytes];
  }
  return value;
}
test('partial-alpha current-color positive requires zero previous identities',()=>{
  assertTemporalFar(analyzeTemporalFar(cutoutControl()),{requireCutout:true});
});
test('backing geometry identities cannot survive a partial-alpha roof',()=>{
  const value=cutoutControl(),frame=value.results.find(f=>f.label==='far-pan');
  const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer);
  view.setUint32(8192+(12+32*10)*16,1<<24,true);frame.motion=[...bytes];
  assert.throws(()=>assertTemporalFar(analyzeTemporalFar(value),{requireCutout:true}),/invalidate/);
});
