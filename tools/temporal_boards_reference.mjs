import assert from 'node:assert/strict';
export function analyzeTemporalBoards(value){
  const [w,h]=value.extent,n=w*h,frames=Object.fromEntries(value.results.map(f=>[f.label,f]));
  const half=v=>{const sign=v&32768?-1:1,e=v>>>10&31,m=v&1023;return sign*(e?2**(e-15)*(1+m/1024):2**-14*m/1024);};
  const session=BigInt(value.session),lo=Number(session&0xffffffffn),hi=Number(session>>32n);
  function motion(frame){
    const bytes=Uint8Array.from(frame.motion),data=new DataView(bytes.buffer);assert.equal(bytes.length,n*24);
    const result={known:0,valid:0,moved:0,wrongSession:0,maximumPixels:0};
    for(let p=0;p<n;p++){
      const at=n*8+p*16;if(data.getUint32(at,true)>>>24!==19)continue;
      result.known++;
      if(data.getUint32(at+4,true)!==lo||data.getUint32(at+8,true)!==hi||!data.getUint32(at+12,true))result.wrongSession++;
      if(half(data.getUint16(p*8+6,true))>0){
        result.valid++;const distance=Math.hypot(half(data.getUint16(p*8,true))*w,half(data.getUint16(p*8+2,true))*h);
        result.maximumPixels=Math.max(result.maximumPixels,distance);if(distance>.25)result.moved++;
      }
    }
    return result;
  }
  const before=frames['boards-pan-reference'].image,after=frames['boards-removed-reference'].image,actual=frames['boards-removed'].image;
  const difference=(a,b,p)=>Math.max(...[0,1,2].map(c=>Math.abs(a[p*4+c]-b[p*4+c])));
  const mask=Array.from({length:n},(_,p)=>difference(before,after,p)>24),witnesses=[];
  for(let p=0;p<n;p++){
    const x=p%w,y=Math.floor(p/w);if(x<2||x>=w-2||y<2||y>=h-2)continue;
    let inside=true;for(let dy=-2;dy<=2;dy++)for(let dx=-2;dx<=2;dx++){
      const q=p+dx+dy*w;if(!mask[q]||[0,1,2].some(c=>Math.abs(after[q*4+c]-after[p*4+c])>2))inside=false;
    }
    if(inside)witnesses.push(p);
  }
  let maxError=0,residual=0,signal=0;
  for(const p of witnesses){maxError=Math.max(maxError,difference(actual,after,p));for(let c=0;c<3;c++){const old=before[p*4+c]-after[p*4+c];residual+=(actual[p*4+c]-after[p*4+c])*old;signal+=old*old;}}
  const pan=frames['boards-pan'],restored=frames['boards-restored'];
  return {backend:value.backend,extent:value.extent,session:value.session,
    staticSamples:frames['boards-static'].presented.samples,static:motion(frames['boards-static']),pan:motion(pan),removed:motion(frames['boards-removed']),reenabled:motion(frames['boards-reenabled']),
    restoredExact:pan.image.every((byte,i)=>byte===restored.image[i]),restoredFrameUnchanged:pan.presented.frame===restored.presented.frame,
    ghost:{witnesses:witnesses.length,maxError,residualFraction:Math.max(0,residual/signal)},
    active:{before:pan.boards.active,after:frames['boards-removed'].boards.active,reenabled:frames['boards-reenabled'].boards.active},
    epochsStable:['geometryEpoch','sourceEpoch'].every(key=>frames['boards-static'].presented[key]===pan.presented[key]),
    complete:value.results.filter(f=>f.image).every(f=>f.gpuTotalComplete),canvas:value.results.filter(f=>f.image).every(f=>f.surfacePresented),
    frames:value.results.length,scope:value.source,mode2Exposed:false,fullProductAccepted:false};
}
export function assertTemporalBoards(result,{requireCanvas=true}={}){
  assert.equal(result.staticSamples,8);assert.equal(result.active.before,3);assert.equal(result.active.after,0);assert.equal(result.active.reenabled,3);
  assert(result.static.known>=16&&result.pan.moved>=16,'actual namespace19 moving source required');
  for(const record of [result.static,result.pan,result.reenabled])assert.equal(record.wrongSession,0,'exact full64 session/lifecycle required');
  assert.equal(result.removed.known,0);assert(result.reenabled.known>=16);assert.equal(result.reenabled.valid,0,'new lifecycle must reject the retired board history');
  assert(result.epochsStable,'camera pan must not substitute a blanket reset');
  assert(result.ghost.witnesses>=16&&result.ghost.maxError<=2&&result.ghost.residualFraction<=.05,'removed board retains history');
  assert(result.restoredExact&&result.restoredFrameUnchanged,'discard must preserve actual presented frame');
  assert(result.complete);if(requireCanvas)assert(result.canvas);
}
