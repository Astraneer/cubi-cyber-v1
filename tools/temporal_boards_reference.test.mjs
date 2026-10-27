import test from 'node:test';
import assert from 'node:assert/strict';
import {analyzeTemporalBoards,assertTemporalBoards} from './temporal_boards_reference.mjs';
function fixture(){
  const w=32,h=32,n=w*h,session=(1n<<40n)|7n;
  const labels=['boards-background-reference','boards-reference','boards-static','boards-pan','boards-pan-reference','boards-restored','boards-removed','boards-removed-reference','boards-reenabled'];
  const results=labels.map((label,index)=>{
    const absent=label.includes('background')||label.includes('removed');
    const image=Array.from({length:n*4},(_,i)=>i%4===3?255:!absent&&i/4%w>=6&&i/4%w<26&&Math.floor(i/4/w)>=6&&Math.floor(i/4/w)<26?200:100);
    const bytes=new Uint8Array(n*24),view=new DataView(bytes.buffer);
    if(!absent)for(let y=6;y<26;y++)for(let x=6;x<26;x++){
      const p=x+w*y,at=n*8+p*16;
      [0x1300000c,7,256,label==='boards-reenabled'?2:1].forEach((v,c)=>view.setUint32(at+c*4,v,true));
      view.setUint16(p*8,label==='boards-pan'?0x2800:0,true);
      view.setUint16(p*8+6,label==='boards-reenabled'?0:0x3c00,true);
    }
    return {label,image,motion:[...bytes],boards:{active:absent?0:3},gpuTotalComplete:true,surfacePresented:true,
      presented:{frame:['boards-pan','boards-restored'].includes(label)?10:index+1,samples:8,geometryEpoch:1,sourceEpoch:1}};
  });
  return {extent:[w,h],results,session:session.toString()};
}
test('presented namespace19 control passes',()=>assertTemporalBoards(analyzeTemporalBoards(fixture())));
test('truncating the high session word is rejected',()=>{
  const value=fixture(),frame=value.results.find(f=>f.label==='boards-pan');
  const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer);view.setUint32(8192+(12+32*12)*16+8,0,true);frame.motion=[...bytes];
  assert.throws(()=>assertTemporalBoards(analyzeTemporalBoards(value)),/full64/);
});
test('removed board ghost is rejected',()=>{
  const value=fixture();value.results.find(f=>f.label==='boards-removed').image=value.results.find(f=>f.label==='boards-pan-reference').image;
  assert.throws(()=>assertTemporalBoards(analyzeTemporalBoards(value)),/retains history/);
});
test('an unpresented image cannot replace the committed cache',()=>{
  const value=fixture();value.results.find(f=>f.label==='boards-restored').image[0]++;
  assert.throws(()=>assertTemporalBoards(analyzeTemporalBoards(value)),/actual presented/);
});
test('re-enabled lifecycle cannot borrow the retired motion',()=>{
  const value=fixture(),frame=value.results.find(f=>f.label==='boards-reenabled');
  const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer);view.setUint16((12+32*12)*8+6,0x3c00,true);frame.motion=[...bytes];
  assert.throws(()=>assertTemporalBoards(analyzeTemporalBoards(value)),/new lifecycle/);
});
