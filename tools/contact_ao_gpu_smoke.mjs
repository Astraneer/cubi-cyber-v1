// Actual Dawn/Tint and same-frame ambient-only MSAA image proof.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const profiling=process.env.CUBI_AO_PROFILE==='1';
const extent=profiling?Number(process.env.CUBI_AO_EXTENT??960):32;
assert.ok(Number.isInteger(extent)&&extent>=32&&extent<=1024&&extent%32===0,'AO profile extent must be32..1024, multiple32');
const ao=await readFile('crates/render/src/contact_ao.wgsl','utf8');
const implementation=await readFile('crates/render/src/contact_ao.rs','utf8');
const kernelBits=[...implementation.match(/const KERNEL_BITS[\s\S]*?=\s*\[([\s\S]*?)\];/)[1].matchAll(/0x[0-9a-f]{8}/g)].map(m=>Number(m[0]));assert.equal(kernelBits.length,128);
const fixture=await readFile('crates/render/tests/contact_ao.rs','utf8');
const scene=fixture.match(/source:\s*wgpu::ShaderSource::Wgsl\(\s*r#"([\s\S]*?)"#\.into\(\)\s*\)/)?.[1];
if(!scene)throw Error('Analytic source fixture missing');
const stats='struct Stats{fragments:atomic<u32>,loads:atomic<u32>,receivers:atomic<u32>,hits:atomic<u32>};@group(0)@binding(4)var<storage,read_write>stats:Stats;fn record_fragment(){atomicAdd(&stats.fragments,1u);}fn record_load(){atomicAdd(&stats.loads,1u);}fn record_receiver(){atomicAdd(&stats.receivers,1u);}fn record_hit(){atomicAdd(&stats.hits,1u);}';
const result=await withBrowser({path:'/contact-ao-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('No WebGPU');const profile=${profiling};const device=await adapter.requestDevice({requiredFeatures:profile&&adapter.features.has('timestamp-query')?['timestamp-query']:[]});const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const B=GPUBufferUsage,T=GPUTextureUsage;const N=${extent};const measured=profile&&device.features.has('timestamp-query');const queries=measured?device.createQuerySet({type:'timestamp',count:2}):null;const resolvedQueries=measured?device.createBuffer({size:256,usage:B.QUERY_RESOLVE|B.COPY_SRC}):null;
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b};
 const texture=(samples,format,copy=false)=>device.createTexture({size:[N,N],sampleCount:samples,format,usage:T.RENDER_ATTACHMENT|T.TEXTURE_BINDING|(copy?T.COPY_SRC:0)});
 const dot=(a,b)=>a.reduce((s,v,i)=>s+v*b[i],0),norm=a=>a.map(v=>v/Math.hypot(...a)),cross=(a,b)=>[a[1]*b[2]-a[2]*b[1],a[2]*b[0]-a[0]*b[2],a[0]*b[1]-a[1]*b[0]];
 const direction=norm([3,3,4]),right=norm(cross([0,1,0],direction)),up=cross(direction,right);
 const vp=new Float32Array(16),inv=new Float32Array(16);for(let i=0;i<3;i++){vp[i*4]=right[i]/3;vp[i*4+1]=up[i]/3;vp[i*4+2]=direction[i]/20;inv[i]=right[i]*3;inv[4+i]=up[i]*3;inv[8+i]=direction[i]*20;inv[12+i]=-direction[i]*10;}vp[14]=.5;vp[15]=1;inv[15]=1;
 const sceneParams=new ArrayBuffer(144),sf=new Float32Array(sceneParams),su=new Uint32Array(sceneParams);sf.set(vp);sf.set(inv,16);
 const params=new ArrayBuffer(672),f=new Float32Array(params),u=new Uint32Array(params);f.set(vp);f.set(inv,16);f.set([1.25,.025,1,0],36);u.set(${JSON.stringify(kernelBits)},40);
 const compile=async code=>{const module=device.createShaderModule({code});const info=await module.getCompilationInfo();const bad=info.messages.filter(m=>m.type==='error');if(bad.length)throw Error(JSON.stringify(bad.map(m=>({line:m.lineNum,message:m.message}))));return module;};
 const source=await compile(${JSON.stringify(scene)}),results=[];
 const half=h=>{const sign=h&32768?-1:1,e=h>>10&31,m=h&1023;return e===0?sign*m*2**-24:sign*(1+m/1024)*2**(e-15)};
 for(const samples of (profile?[4]:[1,4])){
  let code=${JSON.stringify(ao)}.replace('// STATS',profile?'fn record_fragment(){}fn record_load(){}fn record_receiver(){}fn record_hit(){}':${JSON.stringify(stats)});if(samples===1)code=code.replaceAll('texture_depth_multisampled_2d','texture_depth_2d').replaceAll('texture_multisampled_2d','texture_2d');const module=await compile(code);
  const hdr=texture(samples,'rgba16float'),ambient=texture(samples,'rgba16float'),depth=texture(samples,'depth32float'),original=texture(1,'rgba16float',true),target=texture(samples,'rgba16float',samples===1),resolve=samples===1?target:texture(1,'rgba16float',true);
  const sourceBuffer=buffer(144,B.UNIFORM|B.COPY_DST,sceneParams),config=buffer(672,B.UNIFORM|B.COPY_DST,params),statistics=buffer(16,B.STORAGE|B.COPY_SRC|B.COPY_DST);
  const scenePipeline=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:source,entryPoint:'vertex'},fragment:{module:source,entryPoint:'fragment',targets:[{format:'rgba16float'},{format:'rgba16float'}]},depthStencil:{format:'depth32float',depthCompare:'always',depthWriteEnabled:true},multisample:{count:samples}});
  const sceneBind=device.createBindGroup({layout:scenePipeline.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:sourceBuffer}}]});
  const pipelineStart=performance.now();const pipeline=await device.createRenderPipelineAsync({layout:'auto',vertex:{module,entryPoint:'vertex'},fragment:{module,entryPoint:'fragment',targets:[{format:'rgba16float'}]},multisample:{count:samples}});
  const pipelineMs=performance.now()-pipelineStart;const bind=device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:config}},...[[1,hdr],[2,ambient],[3,depth]].map(([binding,t])=>({binding,resource:t.createView()})),...(profile?[]:[{binding:4,resource:{buffer:statistics}}])]});
  // Single-sample inputs need COPY_SRC for the exact source baseline; a separate
  // resolve is used forMSAA4. Copying a multisampled texture is never attempted.
  const singleOriginal=samples===1?texture(1,'rgba16float',true):null;
  const copyModule=samples===1?await compile('@group(0)@binding(0)var source:texture_2d<f32>;@vertex fn v(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}@fragment fn f(@builtin(position)p:vec4<f32>)->@location(0)vec4<f32>{return textureLoad(source,vec2<i32>(p.xy),0);}'):null;
  const copy=samples===1?await device.createRenderPipelineAsync({layout:'auto',vertex:{module:copyModule,entryPoint:'v'},fragment:{module:copyModule,entryPoint:'f',targets:[{format:'rgba16float'}]}}):null;
  const copyBind=samples===1?device.createBindGroup({layout:copy.getBindGroupLayout(0),entries:[{binding:0,resource:hdr.createView()}]}):null;
  for(const quality of (profile?[1]:[1,2]))for(const [mode,direct,signed] of (profile?[[1,1,0],[1,0,0]]:[[0,0,0],[1,0,0],[2,0,0],[1,1,0],[1,0,1]]))for(let repeat=0;repeat<(profile?2:1);repeat++){
   u.set([N,N,samples,quality===1?16:32],32);device.queue.writeBuffer(config,0,params);su.set([mode,direct,signed,0],32);device.queue.writeBuffer(sourceBuffer,0,sceneParams);
   const timingOffset=Math.ceil((N*N*16+16)/256)*256,read=buffer(profile?timingOffset+16:N*N*16+16,B.MAP_READ|B.COPY_DST),started=performance.now(),e=device.createCommandEncoder();e.clearBuffer(statistics);
   let p=e.beginRenderPass({colorAttachments:[{view:hdr.createView(),resolveTarget:samples>1?original.createView():undefined,loadOp:'clear',clearValue:[0,0,0,0],storeOp:'store'},{view:ambient.createView(),loadOp:'clear',clearValue:[0,0,0,0],storeOp:'store'}],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(scenePipeline);p.setBindGroup(0,sceneBind);p.draw(3);p.end();
   if(samples===1){p=e.beginRenderPass({colorAttachments:[{view:singleOriginal.createView(),loadOp:'clear',clearValue:[0,0,0,0],storeOp:'store'}]});p.setPipeline(copy);p.setBindGroup(0,copyBind);p.draw(3);p.end();}
   p=e.beginRenderPass({colorAttachments:[{view:target.createView(),resolveTarget:samples>1?resolve.createView():undefined,loadOp:'clear',clearValue:[0,0,0,0],storeOp:'store'}],timestampWrites:measured?{querySet:queries,beginningOfPassWriteIndex:0,endOfPassWriteIndex:1}:undefined});p.setPipeline(pipeline);p.setBindGroup(0,bind);p.draw(3);p.end();
   const output=samples===1?target:resolve;
   for(const [i,t]of [[0,output],[1,samples===1?singleOriginal:original]])e.copyTextureToBuffer({texture:t},{buffer:read,offset:i*N*N*8,bytesPerRow:N*8},[N,N]);e.copyBufferToBuffer(statistics,0,read,N*N*16,16);if(measured){e.resolveQuerySet(queries,0,2,resolvedQueries,0);e.copyBufferToBuffer(resolvedQueries,0,read,timingOffset,16);}device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const wallMs=performance.now()-started,mapped=read.getMappedRange(),ticks=measured?new BigUint64Array(mapped,timingOffset,2):null,gpuMs=ticks?Number(ticks[1]-ticks[0])/1e6:null,raw=new Uint16Array(mapped),counts=Array.from(new Uint32Array(mapped,N*N*16,4));let changed=0,negative=0;
   for(let i=0;i<N*N*4;i++){const actual=raw[i],baseline=raw[N*N*4+i];if(actual!==baseline)changed++;if(mode!==1||direct){if(actual!==baseline)throw Error(JSON.stringify({samples,quality,mode,direct,i,actual:half(actual),baseline:half(baseline)}));}else if(i%4===0){negative=Math.max(negative,Math.abs(half(baseline)-half(actual)));}}
   if(!profile&&(counts[0]!==N*N*samples||counts[1]>N*N*samples*(5+(quality===1?16:32))))throw Error(JSON.stringify({samples,quality,counts}));if(direct&&counts.slice(1).some(v=>v!==0))throw Error('Direct-only scene readdepth');if(mode===1&&!direct&&(changed<20||negative<.02||(!profile&&counts[3]<10)))throw Error(JSON.stringify({samples,quality,changed,negative,counts}));results.push({extent:N,samples,quality,mode,direct,signed,repeat,changed,negative,counts,statisticsEnabled:!profile,maxDepthReads:N*N*samples*(5+(quality===1?16:32)),pipelineMs,gpuMs,wallMs});read.unmap();
  }
 }
 await device.queue.onSubmittedWorkDone();const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{shaderErrors:0,results};
})()`));
assert.equal(result.results.length,profiling?4:20);await writeFile(profiling?'.local/contact-ao-dawn-profile.json':'.local/contact-ao-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log((profiling?'DAWN_CONTACT_AO_PROFILE ':'DAWN_CONTACT_AO_OK ')+JSON.stringify(result));
