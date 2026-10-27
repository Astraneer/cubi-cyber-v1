import assert from 'node:assert/strict';

// Independent image controls and actual attachment readback, no renderer logic.
export function analyzeTemporalFar(value) {
  const [w,h]=value.extent,n=w*h;
  const frames=Object.fromEntries(value.results.map(frame=>[frame.label,frame]));
  const delta=(a,b,p)=>Math.max(...[0,1,2].map(c=>Math.abs(a[p*4+c]-b[p*4+c])));
  const half=v=>{const s=v&32768?-1:1,e=v>>>10&31,m=v&1023;return s*(e?2**(e-15)*(1+m/1024):2**-14*m/1024);};
  function sourceMotion(frame) {
    const data=new DataView(Uint8Array.from(frame.motion).buffer),families={},moving={},nonzero={};
    assert.equal(data.byteLength,n*24);
    for(let p=0;p<n;p++){
      const family=data.getUint32(n*8+p*16,true)>>>24;
      if(family)families[family]=(families[family]??0)+1;
      if(half(data.getUint16(p*8+6,true))>0&&(half(data.getUint16(p*8,true))!==0||half(data.getUint16(p*8+2,true))!==0))nonzero[family]=(nonzero[family]??0)+1;
      if(half(data.getUint16(p*8+6,true))>0&&Math.hypot(half(data.getUint16(p*8,true))*w,half(data.getUint16(p*8+2,true))*h)>.25)
        moving[family]=(moving[family]??0)+1;
    }
    return {families,moving,nonzero};
  }
  const before=frames['far-pan-reference'].image,after=frames['far-removed-reference'].image;
  const mask=Array.from({length:n},(_,p)=>delta(before,after,p)>24);
  const interior=p=>{
    const x=p%w,y=Math.floor(p/w);if(x<2||x>=w-2||y<2||y>=h-2)return false;
    // Source removal also reveals floor/far-clip edges. A zero-jitter current
    // reference cannot predict their coverage at another subpixel phase.
    // Choose only flat current-background interiors, independently of actual
    // TAA output, identity, or history counters. Ghost thresholds stay fixed.
    for(let dy=-2;dy<=2;dy++)for(let dx=-2;dx<=2;dx++){
      const q=p+dx+dy*w;
      if(!mask[q]||[0,1,2].some(c=>Math.abs(after[q*4+c]-after[p*4+c])>2))return false;
    }
    return true;
  };
  const witnesses=Array.from({length:n},(_,p)=>p).filter(interior);
  let maxError=0,residual=0,signal=0;
  for(const p of witnesses){
    maxError=Math.max(maxError,delta(frames['far-removed'].image,after,p));
    for(let c=0;c<3;c++){const old=before[p*4+c]-after[p*4+c];residual+=(frames['far-removed'].image[p*4+c]-after[p*4+c])*old;signal+=old*old;}
  }
  const cutout={witnesses:0,maxError:0,nonzeroIdentities:0};
  if(value.partialCutoutAlpha===128){
    const actual=frames['far-pan'].image;
    const bytes=Uint8Array.from(frames['far-pan'].motion),data=new DataView(bytes.buffer);
    const green=p=>before[p*4+1]>before[p*4]+16&&before[p*4+1]>before[p*4+2]+16;
    for(let p=0;p<n;p++){
      const x=p%w,y=Math.floor(p/w);if(x<2||x>=w-2||y<2||y>=h-2)continue;
      let inside=true;
      for(let dy=-2;dy<=2;dy++)for(let dx=-2;dx<=2;dx++){
        const q=p+dx+dy*w;
        if(!green(q)||[0,1,2].some(c=>Math.abs(before[q*4+c]-before[p*4+c])>2))inside=false;
      }
      if(!inside)continue;
      cutout.witnesses++;
      cutout.maxError=Math.max(cutout.maxError,delta(actual,before,p));
      if([0,1,2,3].some(c=>data.getUint32(n*8+p*16+c*4,true)!==0))cutout.nonzeroIdentities++;
    }
  }
  return {backend:value.backend,extent:value.extent,committed:value.committed,replaced:value.replaced,
    staticSamples:frames['far-static'].presented.samples,staticMotion:sourceMotion(frames['far-static']),
    panMotion:sourceMotion(frames['far-pan']),removal:{witnesses:witnesses.length,maxError,residualFraction:Math.max(0,residual/signal)},
    geometryEpoch:[frames['far-static'].presented.geometryEpoch,frames['far-pan'].presented.geometryEpoch],
    sourceEpoch:[frames['far-static'].presented.sourceEpoch,frames['far-pan'].presented.sourceEpoch],
    complete:value.results.every(frame=>frame.gpuTotalComplete),canvas:value.results.every(frame=>frame.surfacePresented),
    frames:value.results.length,cutout,scope:value.source,mode2Exposed:false,fullProductAccepted:false};
}

export function assertTemporalFar(result,{requireCanvas=true,requireCutout=false}={}) {
  for(const front of [result.committed,result.replaced])assert(front.committed&&front.guard&&front.heightFront>0&&front.proxyFront===8&&front.knownSources===8,'actual Height/Proxy front required');
  assert.equal(result.staticSamples,8);
  for(const family of [16,17]){
    assert((result.staticMotion.families[family]??0)>=16,`missing actual Far family ${family}`);
    assert.equal(result.staticMotion.nonzero[family]??0,0,'jitter alone must not invent source motion');
    assert((result.panMotion.moving[family]??0)>=16,`Far family ${family} lacks valid camera motion`);
  }
  assert.equal(result.geometryEpoch[0],result.geometryEpoch[1]);
  assert.equal(result.sourceEpoch[0],result.sourceEpoch[1]);
  assert(result.removal.witnesses>=16,'need independent vacated Far interior');
  assert(result.removal.maxError<=2&&result.removal.residualFraction<=.05,'removed public geometry retains history');
  assert(result.complete);if(requireCanvas)assert(result.canvas);
  if(requireCutout){
    assert(result.cutout.witnesses>=16,'actual partial-alpha roof positive control required');
    assert(result.cutout.maxError<=2,'reactive roof must use current color');
    assert.equal(result.cutout.nonzeroIdentities,0,'partial-alpha roof must invalidate previous surface identity');
  }
}
