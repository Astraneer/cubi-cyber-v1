// Real Dawn/Tint and image readback for the production Cloud compute/composite.
// Source inputs are synthetic public optical fixtures; no game state is invented.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const compute=await readFile('crates/render/src/clouds.wgsl','utf8');
const composite=await readFile('crates/render/src/cloud_composite.wgsl','utf8');
const result=await withBrowser({path:'/cloud-msaa-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU adapter missing');
 const device=await adapter.requestDevice(),errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const modules=[];for(const code of[${JSON.stringify(compute)},${JSON.stringify(composite)}]){const m=device.createShaderModule({code});const info=await m.getCompilationInfo();const bad=info.messages.filter(m=>m.type==='error').map(m=>({line:m.lineNum,text:m.message}));if(bad.length)throw Error(JSON.stringify(bad));modules.push(m);}
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b};
 const texture=(format,samples,usage,width=32,height=32)=>device.createTexture({size:[width,height],format,sampleCount:samples,usage});
 const S=GPUShaderStage,T=GPUTextureUsage,B=GPUBufferUsage;
 const sourceColor=texture('rgba16float',4,T.RENDER_ATTACHMENT|T.TEXTURE_BINDING),sourceDepth=texture('depth32float',4,T.RENDER_ATTACHMENT|T.TEXTURE_BINDING),lut=texture('rgba32float',1,T.COPY_DST|T.TEXTURE_BINDING,1,1);
 device.queue.writeTexture({texture:lut},new Float32Array([1,1,1,1]),{bytesPerRow:16},[1,1]);
 const full=texture('rgba16float',1,T.STORAGE_BINDING|T.TEXTURE_BINDING),opaque=texture('rgba16float',1,T.STORAGE_BINDING|T.TEXTURE_BINDING),end=texture('r32float',1,T.STORAGE_BINDING|T.TEXTURE_BINDING);
 const output=texture('rgba16float',4,T.RENDER_ATTACHMENT),resolve=texture('rgba16float',1,T.RENDER_ATTACHMENT|T.COPY_SRC);
 const params=new ArrayBuffer(176),pf=new Float32Array(params),pu=new Uint32Array(params);
 // Inverse reverse-depth perspective, eye0, looking toward+Y, fov0.2.
 // CPU Beer reference below is derived directly from ray/slab length, not this
 // matrix or either production shader implementation.
 pf.set([-Math.tan(.1),0,0,0, 0,0,-Math.tan(.1),0, 0,0,0,9.99, 0,1,0,.01]);
 pf.set([0,0,0,0],16);pf.set([0,1,0,0],20);pf.set([10,10,.5,.15],24);pf.set([0,0,.01,1],28);pu.set([32,32,32,32],32);pu.set([32,0,0,0],36);pf.set([.1,.3,0,100],40);
 const uniform=buffer(176,B.UNIFORM|B.COPY_DST,params),statistics=buffer(16,B.STORAGE|B.COPY_SRC);
 const sampled=(binding,visibility,depth=false,multisampled=false)=>({binding,visibility,texture:{sampleType:depth?'depth':'unfilterable-float',multisampled}});
 const cl=device.createBindGroupLayout({entries:[{binding:0,visibility:S.COMPUTE,buffer:{type:'uniform'}},sampled(1,S.COMPUTE),sampled(2,S.COMPUTE),sampled(3,S.COMPUTE,true,true),...[['rgba16float',4],['rgba16float',5],['r32float',6]].map(([format,binding])=>({binding,visibility:S.COMPUTE,storageTexture:{format,access:'write-only'}})),{binding:7,visibility:S.COMPUTE,buffer:{type:'storage'}}]});
 const cpl=device.createPipelineLayout({bindGroupLayouts:[cl]});
 const reset=await device.createComputePipelineAsync({layout:cpl,compute:{module:modules[0],entryPoint:'reset'}}),march=await device.createComputePipelineAsync({layout:cpl,compute:{module:modules[0],entryPoint:'march'}});
 const cb=device.createBindGroup({layout:cl,entries:[{binding:0,resource:{buffer:uniform}},...[[1,lut],[2,lut],[3,sourceDepth],[4,full],[5,opaque],[6,end]].map(([binding,t])=>({binding,resource:t.createView()})),{binding:7,resource:{buffer:statistics}}]});
 const fl=device.createBindGroupLayout({entries:[{binding:0,visibility:S.FRAGMENT,buffer:{type:'uniform'}},... [1,2,3].map(binding=>sampled(binding,S.FRAGMENT)),sampled(4,S.FRAGMENT,true,true),sampled(5,S.FRAGMENT,false,true)]});
 const fpl=device.createPipelineLayout({bindGroupLayouts:[fl]}),render=await device.createRenderPipelineAsync({layout:fpl,vertex:{module:modules[1],entryPoint:'vertex'},multisample:{count:4},fragment:{module:modules[1],entryPoint:'fragment',targets:[{format:'rgba16float'}]}});
 const bind=wrong=>device.createBindGroup({layout:fl,entries:[{binding:0,resource:{buffer:uniform}},...[[1,wrong?opaque:full],[2,opaque],[3,end],[4,sourceDepth],[5,sourceColor]].map(([binding,t])=>({binding,resource:t.createView()}))]});
 const proper=bind(false),wrong=bind(true);
 const mask=buffer(16,B.UNIFORM|B.COPY_DST);
 const sourceModule=device.createShaderModule({code:'@group(0)@binding(0)var<uniform>mask:vec4<u32>;@vertex fn v(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let a=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(a[i],0.,1.);}struct O{@location(0)c:vec4<f32>,@builtin(frag_depth)d:f32,@builtin(sample_mask)m:u32};@fragment fn f()->O{var o:O;o.c=vec4(1.,0.,0.,1.);o.d=.019019019;o.m=mask.x;return o;}'});
 const source=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:sourceModule,entryPoint:'v'},multisample:{count:4},depthStencil:{format:'depth32float',depthCompare:'always',depthWriteEnabled:true},fragment:{module:sourceModule,entryPoint:'f',targets:[{format:'rgba16float'}]}});
 const sourceBind=device.createBindGroup({layout:source.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:mask}}]});
 const half=h=>{const sign=h&32768?-1:1,e=h>>10&31,m=h&1023;return e===0?sign*m*2**-24:sign*(1+m/1024)*2**(e-15)};
 const capture=async(quality,sampleMask,useWrong=false)=>{
  pu[38]=quality;device.queue.writeBuffer(uniform,0,params);device.queue.writeBuffer(mask,0,new Uint32Array([sampleMask,0,0,0]));
  const e=device.createCommandEncoder();let p=e.beginRenderPass({colorAttachments:[{view:sourceColor.createView(),clearValue:[0,0,1,1],loadOp:'clear',storeOp:'store'}],depthStencilAttachment:{view:sourceDepth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(source);p.setBindGroup(0,sourceBind);p.draw(3);p.end();
  for(const pipeline of[reset,march]){p=e.beginComputePass();p.setPipeline(pipeline);p.setBindGroup(0,cb);p.dispatchWorkgroups(pipeline===reset?1:4,pipeline===reset?1:4);p.end();}
  p=e.beginRenderPass({colorAttachments:[{view:output.createView(),resolveTarget:resolve.createView(),clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'}]});p.setPipeline(render);p.setBindGroup(0,useWrong?wrong:proper);p.draw(3);p.end();
  const pixels=buffer(8192,B.MAP_READ|B.COPY_DST),counts=buffer(16,B.MAP_READ|B.COPY_DST);e.copyTextureToBuffer({texture:resolve},{buffer:pixels,bytesPerRow:256},[32,32]);e.copyBufferToBuffer(statistics,0,counts,0,16);device.queue.submit([e.finish()]);await Promise.all([pixels.mapAsync(GPUMapMode.READ),counts.mapAsync(GPUMapMode.READ)]);const raw=new Uint16Array(pixels.getMappedRange().slice(0)),stats=[...new Uint32Array(counts.getMappedRange().slice(0))];pixels.unmap();counts.unmap();pixels.destroy();counts.destroy();return{raw:[...raw],rgba:Array.from({length:1024},(_,i)=>Array.from({length:4},(_,c)=>half(raw[i*4+c]))),stats};
 };
 const results=[];let a;
 for(const quality of[0,1]){
  const image=await capture(quality,5);const steps=quality?32:1;const expectedStats=[1024*steps,1024*steps,1024,1024];if(JSON.stringify(image.stats)!==JSON.stringify(expectedStats))throw Error(JSON.stringify({quality,stats:image.stats,expectedStats}));let error=0;
  for(let y=0;y<32;y++)for(let x=0;x<32;x++){const nx=(x+.5)/16-1,ny=(y+.5)/16-1,length=Math.sqrt(1+(nx*nx+ny*ny)*Math.tan(.1)**2),expected=Math.exp(-length)*.5,p=image.rgba[x+32*y];if(p.some(v=>!Number.isFinite(v))||Math.abs(p[0]-.5)>.001||Math.abs(p[1])>.001||p[3]!==1)throw Error(JSON.stringify({quality,x,y,p}));error=Math.max(error,Math.abs(p[2]-expected));}
  if(error>.002)throw Error('Beer error '+error);if(a&&JSON.stringify(a)!==JSON.stringify(image.raw))throw Error('Uniform slab depends on LayerA/B step count');a=image.raw;
  const allOpaque=await capture(quality,15);if(allOpaque.rgba.some(p=>p[0]!==1||p[1]!==0||p[2]!==0||p[3]!==1))throw Error('Cloud behind opaque foreground leaked');
  const bad=await capture(quality,5,true);let negative=0;for(let i=0;i<1024;i++)negative=Math.max(negative,Math.abs(bad.rgba[i][2]-image.rgba[i][2]));if(negative<.1)throw Error('Wrong opaque-only cloud source escaped independent oracle');results.push({quality:quality?'VolumeB':'LayerA',pixels:1024,stats:image.stats,maxBeerError:error,foregroundSamplesPreserved:2,opaqueForegroundExact:true,uniformStepInvariant:true,negativeOpaqueOnlyError:negative});
 }
 await device.queue.onSubmittedWorkDone();const validation=await device.popErrorScope();if(validation)throw Error(validation.message);if(errors.length)throw Error(JSON.stringify(errors));device.destroy();return{adapter:adapter.info.description,pipelines:4,shaderErrors:0,results};
})()`));
assert.equal(result.results.length,2);
await writeFile('.local/cloud-dawn-proof.json',JSON.stringify(result,null,2)+'\n');
console.log('DAWN_CLOUD_GPU_OK '+JSON.stringify(result));
