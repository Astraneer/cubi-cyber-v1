import test from 'node:test';
import assert from 'node:assert/strict';
import {analyzeTemporalView,assertTemporalViewProof} from './temporal_view_reference.mjs';
function fixture(){
  const w=24,h=24,pixels=w*h;
  const bg=Array.from({length:pixels},()=>[40,60,80,255]).flat();
  const paint=(x0,x1,color)=>{const image=[...bg];for(let y=4;y<20;y++)for(let x=x0;x<x1;x++)image.splice((x+w*y)*4,4,...color,255);return image;};
  const old=paint(2,10,[220,90,20]),current=paint(14,22,[190,80,20]),msaa=[...old];
  for(let y=4;y<20;y++)for(let x=2;x<10;x++)if(x===2||x===9||y===4||y===19){const p=(x+w*y)*4;for(let c=0;c<3;c++)msaa[p+c]=Math.round((old[p+c]+bg[p+c])/2);}
  const motion=new Uint8Array(pixels*24),dv=new DataView(motion.buffer);
  for(let y=4;y<20;y++)for(let x=14;x<22;x++){const p=x+w*y;dv.setUint16(p*8,0x3000,true);dv.setUint16(p*8+6,0x3c00,true);dv.setUint32(pixels*8+p*16,9<<24,true);}
  const frames=[['reference-background',bg],['reference-old',old],['reference-new',current],['reference-msaa4',msaa],['warmup',msaa],['background-warmup',bg],['moved',current],['background-moved',bg],['restored',current],['removed',bg],['background-removed',bg],['full-graph',msaa],['full-cached',msaa],['full-restored',msaa]].map(([label,image])=>({label,image:[...image],motion:Array.from(motion),counters:[pixels,pixels*10,pixels,0],presented:{frame:label==='warmup'?1:2,samples:8,geometryEpoch:1,sourceEpoch:1},gpuTotalComplete:true,surfacePresented:true,sources:15,queries:45}));
  const deformed=structuredClone(frames.find(f=>f.label==='moved'));deformed.label='deformed';frames.push(deformed);
  return {extent:[w,h],results:frames,backend:'authored independent image-control test',source:'oracle math only'};
}
test('independent current/background controls accept a clean translated silhouette',()=>{
  const result=analyzeTemporalView(fixture());assertTemporalViewProof(result);
  assert.equal(result.moved.maxError,0);assert.equal(result.quality.taaError,0);
  assert(result.moved.witnesses>=16&&result.sourceMotion.movingHuman>=16);
});
test('a trailing old silhouette is rejected even when restore and motion are correct',()=>{
  const value=fixture(),frames=Object.fromEntries(value.results.map(f=>[f.label,f]));
  for(let y=4;y<20;y++)for(let x=2;x<10;x++)for(let c=0;c<3;c++){
    const p=(x+24*y)*4+c;frames.moved.image[p]=Math.round((frames['reference-old'].image[p]+frames['reference-background'].image[p])/2);
  }
  frames.restored.image=[...frames.moved.image];
  const result=analyzeTemporalView(value);assert(result.restoreExact);
  assert(result.moved.residualFraction>.45);
  assert.throws(()=>assertTemporalViewProof(result),/vacated model interior/);
});
test('camera-only zero vectors cannot pass the actual moving-model requirement',()=>{
  const value=fixture(),moved=value.results.find(f=>f.label==='moved');moved.motion.fill(0);
  assert.throws(()=>assertTemporalViewProof(analyzeTemporalView(value)),/nonzero valid motion/);
});
test('a submitted Post/history rollback that changes one pixel is rejected',()=>{
  const value=fixture(),restored=value.results.find(f=>f.label==='full-restored');restored.image[0]++;
  assert.throws(()=>assertTemporalViewProof(analyzeTemporalView(value)),/restore the last actual presented image exactly/);
});
test('rigid object vectors cannot substitute for a missing bone-motion pass',()=>{
  const value=fixture(),deformed=value.results.find(f=>f.label==='deformed');deformed.motion.fill(0);
  assert.throws(()=>assertTemporalViewProof(analyzeTemporalView(value)),/nonzero bone motion/);
});
