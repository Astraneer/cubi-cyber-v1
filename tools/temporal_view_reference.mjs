import assert from 'node:assert/strict';

export function analyzeTemporalView(value) {
  const [w,h]=value.extent, pixels=w*h;
  const byName=Object.fromEntries(value.results.map(frame=>[frame.label,frame]));
  const image=name=>{const result=byName[name];assert.equal(result.image.length,pixels*4);return result.image;};
  const distance=(a,b,p)=>Math.max(...[0,1,2].map(c=>Math.abs(a[p*4+c]-b[p*4+c])));
  const bg=image('reference-background'),old=image('reference-old'),current=image('reference-new');
  const oldMask=Array.from({length:pixels},(_,p)=>distance(old,bg,p)>24);
  const newMask=Array.from({length:pixels},(_,p)=>distance(current,bg,p)>24);
  const interior=(mask,p,radius=2)=>{
    const x=p%w,y=Math.floor(p/w);if(x<radius||x>=w-radius||y<radius||y>=h-radius)return false;
    for(let dy=-radius;dy<=radius;dy++)for(let dx=-radius;dx<=radius;dx++)if(!mask[p+dx+dy*w])return false;
    return true;
  };
  function ghost(actualName,backgroundName,mask) {
    const actual=image(actualName),background=image(backgroundName),region=[];
    for(let p=0;p<pixels;p++)if(interior(mask,p)&&distance(current,bg,p)<2)region.push(p);
    assert(region.length>=16,`${actualName}: insufficient independent interior witnesses ${region.length}`);
    let maxError=0,residual=0,signal=0;
    for(const p of region){maxError=Math.max(maxError,distance(actual,background,p));
      for(let c=0;c<3;c++){const before=old[p*4+c]-bg[p*4+c];residual+=(actual[p*4+c]-background[p*4+c])*before;signal+=before*before;}}
    const residualFraction=Math.max(0,residual/signal);
    return {witnesses:region.length,maxError,residualFraction};
  }
  const moved=ghost('moved','background-moved',oldMask);
  const removedActual=image('removed'),removedBackground=image('background-removed');
  const removedPixels=Array.from({length:pixels},(_,p)=>p).filter(p=>interior(newMask,p));
  assert(removedPixels.length>=16);
  const removed={witnesses:removedPixels.length,maxError:Math.max(...removedPixels.map(p=>distance(removedActual,removedBackground,p)))};
  function exact(a,b){const first=image(a),second=image(b);return first.every((v,i)=>v===second[i]);}
  const restoreExact=exact('moved','restored'),fullRestoreExact=exact('full-cached','full-restored');
  const half=word=>{const sign=word&32768?-1:1,e=word>>>10&31,m=word&1023;return sign*(e?e===31?m?NaN:Infinity:2**(e-15)*(1+m/1024):2**-14*m/1024);};
  function motion(frame){
    const bytes=Uint8Array.from(frame.motion),view=new DataView(bytes.buffer), families={};let known=0,valid=0,moved=0,movingHuman=0;
    assert.equal(bytes.length,pixels*24);
    for(let p=0;p<pixels;p++){
      const family=view.getUint32(pixels*8+p*16,true)>>>24;
      if(family){known++;families[family]=(families[family]??0)+1;}
      const dx=half(view.getUint16(p*8,true)),dy=half(view.getUint16(p*8+2,true)),ok=half(view.getUint16(p*8+6,true));
      if(ok>0){valid++;if(Math.hypot(dx*w,dy*h)>.25){moved++;if(family===9)movingHuman++;}}
    }
    return {known,valid,moved,movingHuman,families};
  }
  const sourceMotion=motion(byName.moved);
  const boneMotion=motion(byName.deformed);
  const staticSamples=byName.warmup.presented.samples;
  for(const key of ['geometryEpoch','sourceEpoch'])assert.equal(byName.warmup.presented[key],byName.moved.presented[key],`${key}: move must not fake an unconditional history reset`);
  assert.equal(byName.moved.presented.frame,byName.restored.presented.frame);
  assert(byName.moved.counters[2]>0,'actual history must contribute');
  const msaa=image('reference-msaa4'),taa=image('warmup');let rawError=0,taaError=0,qualityPixels=0;
  for(let p=0;p<pixels;p++)if(oldMask[p]||distance(msaa,bg,p)>8){qualityPixels++;for(let c=0;c<3;c++){rawError+=Math.abs(old[p*4+c]-msaa[p*4+c]);taaError+=Math.abs(taa[p*4+c]-msaa[p*4+c]);}}
  const full=byName['full-graph'];
  const result={backend:value.backend,extent:value.extent,moved,removed,sourceMotion,boneMotion,restoreExact,fullRestoreExact,
    quality:{reference:'actual MSAA4 same geometry/camera/lighting',pixels:qualityPixels,rawError,taaError,ratio:taaError/rawError},
    fullGraph:{sources:full.sources,queries:full.queries,gpuTotalComplete:full.gpuTotalComplete,activeBoards:full.boards?.active??0},
    staticSamples,frames:value.results.length,presentedFrames:value.results.filter(f=>f.gpuTotalComplete).length,
    actualCanvas:value.results.filter(f=>f.gpuTotalComplete).every(f=>f.surfacePresented),
    scope:value.source,mode2Exposed:false,fullProductAccepted:false};
  return result;
}

export function assertTemporalViewProof(result,{requireCanvas=true}={}) {
assert.equal(result.staticSamples,8,'static scene must accumulate eight real phases');
assert(result.moved.maxError<=2&&result.moved.residualFraction<=.05,'vacated model interior retains history');
assert(result.removed.maxError<=2,'removed model interior retains history');
assert(result.sourceMotion.movingHuman>=16,'actual moving Human needs nonzero valid motion under a static camera');
assert(result.boneMotion.movingHuman>=16,'actual deformed Human needs nonzero bone motion without object or camera translation');
assert(result.restoreExact&&result.fullRestoreExact,'discard must restore the last actual presented image exactly');
assert(result.quality.taaError<result.quality.rawError,'actual static TAA must improve the same-scene MSAA4 coverage reference');
assert(result.fullGraph.sources<=16&&result.fullGraph.queries<=64&&result.fullGraph.gpuTotalComplete);

if(requireCanvas)assert(result.actualCanvas,'actual canvas presentation required');
}
