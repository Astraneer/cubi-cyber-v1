// Actual Dawn GPU and Worker acceptance for optional M10 paths.
// Reads only this project's own shaders and local built WASM. No remote service.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';

{
const shaderRoot='crates/render/src/';
const [atm,shadow,surface,dda]=await Promise.all(['atmosphere_lighting.wgsl','shadow_sampling.wgsl','gi_directional.wgsl','gi.wgsl'].map(n=>readFile(shaderRoot+n,'utf8')));
const code=atm.replaceAll('@binding(20)','@binding(7)')+'\n'+shadow.replaceAll('@binding(9)','@binding(8)').replaceAll('@binding(10)','@binding(9)').replaceAll('@binding(11)','@binding(10)')+'\n'+surface+'\n'+dda;
const result=await withBrowser({path:'/directional-gi-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU adapter absent');
 const device=await adapter.requestDevice();const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));
 const module=device.createShaderModule({code:${JSON.stringify(code)}});const info=await module.getCompilationInfo();const messages=info.messages.map(m=>({type:m.type,line:m.lineNum,text:m.message}));if(messages.some(m=>m.type==='error'))throw Error(JSON.stringify(messages));
 const buffer=(size,usage,data)=>{let b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b};
 const entry=(binding,uniform,readOnly)=>({binding,visibility:GPUShaderStage.COMPUTE,buffer:{type:uniform?'uniform':readOnly?'read-only-storage':'storage'}});
 const layout=device.createBindGroupLayout({entries:[entry(0,true,true),entry(1,false,true),entry(2,false,false),entry(3,false,true),entry(4,false,true),entry(5,false,false),entry(6,false,true),entry(7,false,true),entry(8,true,true),{binding:9,visibility:GPUShaderStage.COMPUTE,texture:{sampleType:'depth',viewDimension:'2d-array'}},{binding:10,visibility:GPUShaderStage.COMPUTE,sampler:{type:'comparison'}}]});
 const pl=device.createPipelineLayout({bindGroupLayouts:[layout]});const reset=await device.createComputePipelineAsync({layout:pl,compute:{module,entryPoint:'reset_statistics'}});const trace=await device.createComputePipelineAsync({layout:pl,compute:{module,entryPoint:'trace_probes'}});
 const params=new ArrayBuffer(96),p=new Float32Array(params),u=new Uint32Array(params);p.set([8,4,8,1]);u.set([1,1,1,0],4);u.set([1,1,1,0],12);p[16]=24;u.set([1,64,1,0],20);
 const voxels=new Uint32Array(4096).fill(0x80000000),faces=new Uint32Array(4096*12);for(let z=4;z<12;z++)for(let x=4;x<12;x++){let i=x+16*z;voxels[i]=0xc0000000;faces[i*12+2]=0x1f808080;}
 const probe=new ArrayBuffer(128);new Float32Array(probe).set([8,4,8,1]);new Uint32Array(probe).set([1,0,0,0],28);
 const rays=new Float32Array(256);for(let i=0;i<64;i++){let y=1-2*(i+.5)/64,r=Math.sqrt(1-y*y),angle=i*Math.PI*(3-Math.sqrt(5));rays.set([r*Math.cos(angle),y,r*Math.sin(angle),0],i*4);}
 const light=new Float32Array(20);light[0]=1/.2820947918;light[17]=1;const shadowParams=new Uint32Array(104);shadowParams[101]=1;
 const buffers=[buffer(96,GPUBufferUsage.UNIFORM|GPUBufferUsage.COPY_DST,params),buffer(voxels.byteLength,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,voxels),buffer(128,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST|GPUBufferUsage.COPY_SRC,probe),buffer(16,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,new Uint32Array([0,1,1,64])),buffer(1024,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,rays),buffer(16,GPUBufferUsage.STORAGE),buffer(faces.byteLength,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,faces),buffer(80,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,light),buffer(416,GPUBufferUsage.UNIFORM|GPUBufferUsage.COPY_DST,shadowParams)];
 const depth=device.createTexture({size:[1,1,3],format:'depth32float',usage:GPUTextureUsage.TEXTURE_BINDING|GPUTextureUsage.RENDER_ATTACHMENT});const sampler=device.createSampler({compare:'greater-equal'});
 const bind=device.createBindGroup({layout,entries:[...buffers.map((b,binding)=>({binding,resource:{buffer:b}})),{binding:9,resource:depth.createView({dimension:'2d-array'})},{binding:10,resource:sampler}]});
 const encoder=device.createCommandEncoder();for(const pipeline of[reset,trace]){const pass=encoder.beginComputePass();pass.setPipeline(pipeline);pass.setBindGroup(0,bind);pass.dispatchWorkgroups(1);pass.end();}
 const read=buffer(128,GPUBufferUsage.MAP_READ|GPUBufferUsage.COPY_DST);encoder.copyBufferToBuffer(buffers[2],0,read,0,128);device.queue.submit([encoder.finish()]);await read.mapAsync(GPUMapMode.READ);const actual=[...new Float32Array(read.getMappedRange().slice(0))];read.unmap();
 const expected=[0,0,0,0];let hits=0;for(let i=0;i<64;i++){let d=rays.subarray(i*4,i*4+3);if(d[1]>=0)continue;let t=-3/d[1],x=8+t*d[0],z=8+t*d[2];if(t>=24||x<4||x>=12||z<4||z>=12)continue;hits++;let basis=[.2820947918,.4886025119*d[1],.4886025119*d[2],.4886025119*d[0]];for(let k=0;k<4;k++)expected[k]+=(128/255)*basis[k]*(k===0?Math.PI:2*Math.PI/3)*4*Math.PI/64;}
 const error=Math.max(...expected.map((v,k)=>Math.abs(v-actual[4+k])));if(errors.length)throw Error(JSON.stringify(errors));if(error>1e-5||hits===0||actual[4]<=0||actual.slice(12,16).some(v=>v!==0))throw Error(JSON.stringify({actual,expected,error,hits}));device.destroy();return {adapter:adapter.info.description,storageLimit:adapter.limits.maxStorageBuffersPerShaderStage,pipelines:2,completedProbes:1,hits,maxSHError:error,blueLeak:0,messages};
})()`));
assert(result.completedProbes===1);console.log('DAWN_DIRECTIONAL_GI_GPU_OK '+JSON.stringify(result));
}

for (const sampleCount of [1, 4]) {
const rawCode=(await readFile('crates/render/src/water.wgsl','utf8')).replace('// SHADOWS','fn cascade_shadow(p:vec3<f32>,n:vec3<f32>)->f32{return 1.;}').replace('// STATS','fn record(steps:u32,hit:bool){}');
const code=sampleCount===1?rawCode.replaceAll('texture_depth_multisampled_2d','texture_depth_2d').replaceAll('texture_multisampled_2d','texture_2d'):rawCode;
const result=await withBrowser({path:'/water-snell-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const sampleCount=${sampleCount};
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('No actual WebGPU adapter');const device=await adapter.requestDevice();const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));
 const shader=device.createShaderModule({code:${JSON.stringify(code)}});const info=await shader.getCompilationInfo();if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages.map(m=>({line:m.lineNum,type:m.type,message:m.message}))));
 const F=Math.fround,dot=(a,b)=>a.reduce((s,x,i)=>F(s+F(x*b[i])),0),cross=(a,b)=>[F(F(a[1]*b[2])-F(a[2]*b[1])),F(F(a[2]*b[0])-F(a[0]*b[2])),F(F(a[0]*b[1])-F(a[1]*b[0]))],norm=a=>{let n=F(Math.sqrt(dot(a,a)));return a.map(x=>F(x/n))};
 const eye=[8,-2,8],forward=norm([3,3,0]),right=norm(cross(forward,[0,0,-1])),up=cross(right,forward),view=[[right[0],up[0],-forward[0],0],[right[1],up[1],-forward[1],0],[right[2],up[2],-forward[2],0],[-dot(right,eye),-dot(up,eye),dot(forward,eye),1]];const f=F(1/F(Math.tan(.5))),near=F(.1),far=F(100),projection=[[f,0,0,0],[0,f,0,0],[0,0,F(near/F(far-near)),-1],[0,0,F(F(far*near)/F(far-near)),0]],matrix=Array.from({length:4},(_,c)=>Array.from({length:4},(_,r)=>dot(projection.map(v=>v[r]),view[c])));
 const augmented=Array.from({length:4},(_,r)=>[...matrix.map(v=>v[r]),...Array.from({length:4},(_,c)=>+(r===c))]);for(let c=0;c<4;c++){let pivot=c;for(let r=c+1;r<4;r++)if(Math.abs(augmented[r][c])>Math.abs(augmented[pivot][c]))pivot=r;[augmented[pivot],augmented[c]]=[augmented[c],augmented[pivot]];let d=augmented[c][c];augmented[c]=augmented[c].map(v=>v/d);for(let r=0;r<4;r++)if(r!==c){let scale=augmented[r][c];augmented[r]=augmented[r].map((v,k)=>v-scale*augmented[c][k]);}}const inverse=Array.from({length:4},(_,c)=>Array.from({length:4},(_,r)=>F(augmented[r][c+4])));
 const params=new ArrayBuffer(256),pf=new Float32Array(params),pu=new Uint32Array(params);pf.set(matrix.flat());pf.set(inverse.flat(),16);pf.set([...eye,0],32);pf.set([1.333,.08,128,0],44);pf.set([0,1,0,0],48);pu.set([32,32,1,2],52);pu.set([0,1,0,0],56);
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b};
 const objects=new ArrayBuffer(80);new Float32Array(objects).set([1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]);
 const buffers=[buffer(256,GPUBufferUsage.UNIFORM|GPUBufferUsage.COPY_DST,params),buffer(80,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,objects),buffer(16,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,new Uint32Array([(16<<10)|(1<<15)|(15<<18)|(15<<22),0xffffffff,0xffffff,0])),buffer(4,GPUBufferUsage.STORAGE|GPUBufferUsage.COPY_DST,new Uint32Array([0]))];
 const tex=(format,sampleCount,usage,width=32,height=32)=>device.createTexture({size:[width,height],format,sampleCount,usage});
 const sourceColor=tex('rgba16float',sampleCount,GPUTextureUsage.RENDER_ATTACHMENT|GPUTextureUsage.TEXTURE_BINDING),sourceDepth=tex('depth32float',sampleCount,GPUTextureUsage.RENDER_ATTACHMENT|GPUTextureUsage.TEXTURE_BINDING),sky=tex('rgba32float',1,GPUTextureUsage.COPY_DST|GPUTextureUsage.TEXTURE_BINDING,1,1),snapshot=tex('rgba16float',1,GPUTextureUsage.TEXTURE_BINDING),color=tex('rgba16float',sampleCount,GPUTextureUsage.RENDER_ATTACHMENT|GPUTextureUsage.COPY_SRC),depth=tex('depth32float',sampleCount,GPUTextureUsage.RENDER_ATTACHMENT),resolve=tex('rgba16float',1,GPUTextureUsage.RENDER_ATTACHMENT|GPUTextureUsage.COPY_SRC);
 device.queue.writeTexture({texture:sky},new Float32Array([1,1,1,1]),{bytesPerRow:16},[1,1]);
 const S=GPUShaderStage,entries=[{binding:0,visibility:S.VERTEX|S.FRAGMENT,buffer:{type:'uniform'}},...buffers.slice(1).map((_,i)=>({binding:i+1,visibility:S.VERTEX,buffer:{type:'read-only-storage'}})),...[4,5,6,7,12].map(binding=>({binding,visibility:S.FRAGMENT,texture:{sampleType:binding===6?'depth':'unfilterable-float',multisampled:sampleCount>1&&(binding===6||binding===12)}}))],layout=device.createBindGroupLayout({entries}),pl=device.createPipelineLayout({bindGroupLayouts:[layout]});
 const bindings=[...buffers.map((b,binding)=>({binding,resource:{buffer:b}})),...[[4,sky],[5,snapshot],[6,sourceDepth],[7,sky],[12,sourceColor]].map(([binding,t])=>({binding,resource:t.createView()}))],bind=device.createBindGroup({layout,entries:bindings});
 const make=background=>device.createRenderPipelineAsync({layout:pl,vertex:{module:shader,entryPoint:background?'background_vertex':'vertex'},primitive:{topology:background?'triangle-list':'triangle-strip',cullMode:'none'},depthStencil:{format:'depth32float',depthWriteEnabled:true,depthCompare:background?'always':'greater-equal'},multisample:{count:sampleCount},fragment:{module:shader,entryPoint:background?'background_fragment':'fragment',targets:[{format:'rgba16float'}]}});const bg=await make(true),water=await make(false);
 let encoder=device.createCommandEncoder();let pass=encoder.beginRenderPass({colorAttachments:[{view:sourceColor.createView(),clearValue:[1,1,1,1],loadOp:'clear',storeOp:'store'}],depthStencilAttachment:{view:sourceDepth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});pass.end();device.queue.submit([encoder.finish()]);
 const capture=async underwater=>{pu[57]=+underwater;device.queue.writeBuffer(buffers[0],0,params);const read=buffer(32*32*8,GPUBufferUsage.MAP_READ|GPUBufferUsage.COPY_DST);const e=device.createCommandEncoder(),p=e.beginRenderPass({colorAttachments:[{view:color.createView(),resolveTarget:sampleCount>1?resolve.createView():undefined,clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'}],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setBindGroup(0,bind);p.setPipeline(bg);p.draw(3);p.setPipeline(water);p.draw(4,1);p.end();e.copyTextureToBuffer({texture:sampleCount>1?resolve:color},{buffer:read,bytesPerRow:256},[32,32]);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const bytes=read.getMappedRange().slice(0);read.unmap();read.destroy();const u=new Uint16Array(bytes),half=h=>{let sign=h&32768?-1:1,exp=h>>10&31,m=h&1023;return exp===0?sign*m*2**-24:sign*(1+m/1024)*2**(exp-15)};return Array.from({length:1024},(_,i)=>half(u[i*4]));};
 const actual=await capture(true),samples=sampleCount===1?[[.5,.5]]:[[.375,.125],[.875,.375],[.125,.625],[.625,.875]],transform=v=>Array.from({length:4},(_,r)=>inverse.reduce((s,c,k)=>s+c[r]*v[k],0));let maxError=0,transmitting=0,tir=[];
 for(let y=0;y<32;y++)for(let x=0;x<32;x++){let expected=0,allTir=true,valid=true;for(const sample of samples){const q=transform([(x+sample[0])/16-1,1-(y+sample[1])/16,0,1]),r=[q[0]/q[3]-8,q[1]/q[3]+2,q[2]/q[3]-8],length=Math.hypot(...r),d=r.map(v=>v/length),distance=2/d[1],p=[8+d[0]*distance,8+d[2]*distance];if(d[1]<=0||p.some(v=>v<.1||v>15.9)){valid=false;break}const c=d[1],s2=1.333**2*(1-c*c);if(s2>=1)continue;allTir=false;const ct=Math.sqrt(1-s2),rs=(1.333*c-ct)/(1.333*c+ct),rp=(1.333*ct-c)/(1.333*ct+c);expected+=(1-(rs*rs+rp*rp)/2)/samples.length;}if(!valid)continue;let i=x+32*y;maxError=Math.max(maxError,Math.abs(actual[i]-expected));if(allTir){tir.push(i);if(actual[i]!==0)throw Error('TIR transmission '+JSON.stringify({x,y,color:actual[i]}));}else transmitting++;}
 const wrong=await capture(false);if(errors.length)throw Error(JSON.stringify(errors));if(maxError>.003||transmitting<100||tir.length<100||tir.some(i=>wrong[i]<=.005))throw Error(JSON.stringify({maxError,transmitting,totalInternal:tir.length}));device.destroy();return {sampleCount,pipelines:2,pixels:1024,transmitting,totalInternal:tir.length,maxHDRerror:maxError,criticalAngleDegrees:Math.asin(1/1.333)*180/Math.PI,negativeWrongMedium:true,shaderErrors:info.messages.filter(m=>m.type==='error').length};
})()`));
assert(result.totalInternal>100);console.log('DAWN_WATER_SNELL_GPU_OK '+JSON.stringify(result));
}

{
const result=await withBrowser({path:'/water-worker-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const results=[];
 for(const transport of ['transfer','shared']) {
  if(transport==='shared'&&(!crossOriginIsolated||typeof SharedArrayBuffer!=='function'))throw Error('isolated SAB transport unavailable');
  const worker=new Worker('/workers/jobs.js',{type:'module'});
  const call=(data,transfer=[])=>new Promise((resolve,reject)=>{let timer=setTimeout(()=>reject(Error('water worker deadline')),12000);worker.onmessage=e=>{clearTimeout(timer);resolve(e.data)};worker.onerror=e=>{clearTimeout(timer);reject(Error(e.message))};worker.postMessage(data,transfer)});
  try {
   let ready=await call({type:'init',module:location.origin+'/pkg/client_web.js',export:'execute_water_task'});if(ready.type!=='ready')throw Error(JSON.stringify(ready));
   const request=new Uint8Array(7104),v=new DataView(request.buffer);request.set([67,87,74,49]);v.setBigUint64(4,1n,true);v.setUint32(12,24576,true);for(let i=0;i<27;i++)v.setBigUint64(32+i*8,1n,true);request[248+5*324+9*18+9]=8;for(let i=0;i<256;i++)v.setUint32(6080+i*4,0xffffffff,true);
   const data=()=>{const payload=transport==='shared'?new SharedArrayBuffer(7104):new ArrayBuffer(7104);new Uint8Array(payload).set(request);const control=transport==='shared'?new SharedArrayBuffer(4):undefined;return {type:'execute',id:'4294967297',transport,payload,control,outputLimit:393728}};
   let task=data(),reply=await call(task,transport==='transfer'?[task.payload]:[]);if(reply.type!=='result'||reply.id!==task.id)throw Error(JSON.stringify(reply));
   const bytes=new Uint8Array(reply.payload),output=new DataView(reply.payload);if(String.fromCharCode(...bytes.slice(0,4))!=='CWR1'||bytes[12]!==0)throw Error('invalid water worker response');
   const cells=Number(output.getBigUint64(261,true)),quads=output.getUint32(293,true),frameVertices=Number(output.getBigUint64(285,true));if(cells!==4096||quads!==6||frameVertices!==0||bytes.length!==393)throw Error(JSON.stringify({cells,quads,frameVertices,bytes:bytes.length}));
   task=data();task.outputLimit=1;const bounded=await call(task,transport==='transfer'?[task.payload]:[]);if(bounded.type!=='error'||!bounded.error.includes('byte limit'))throw Error(JSON.stringify(bounded));
   let cancellation=false;if(transport==='shared'){task=data();Atomics.store(new Int32Array(task.control),0,1);const cancelled=await call(task);if(cancelled.type!=='cancelled')throw Error(JSON.stringify(cancelled));cancellation=true;}
   results.push({transport,requestBytes:7104,responseBytes:393,cells,quads,frameVertices,outputBound:true,cancellation});
  }finally{worker.terminate()}
 }
 return results;
})()`));
assert.equal(result.length,2);console.log('BROWSER_WATER_WORKERS_OK '+JSON.stringify(result));
}

{
const result=await withBrowser({path:'/gi-source-worker-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const results=[];
 for(const transport of ['transfer','shared']) {
  if(transport==='shared'&&(!crossOriginIsolated||typeof SharedArrayBuffer!=='function'))throw Error('isolated GI SAB unavailable');
  const worker=new Worker('/workers/jobs.js',{type:'module'});
  const call=(data,transfer=[])=>new Promise((resolve,reject)=>{let timer=setTimeout(()=>reject(Error('GI source worker deadline')),12000);worker.onmessage=e=>{clearTimeout(timer);resolve(e.data)};worker.onerror=e=>{clearTimeout(timer);reject(Error(e.message))};worker.postMessage(data,transfer)});
  try {
   const ready=await call({type:'init',module:location.origin+'/pkg/client_web.js',export:'execute_gi_task'});if(ready.type!=='ready')throw Error(JSON.stringify(ready));
   const request=new Uint8Array(37919),v=new DataView(request.buffer);let at=0;
   const u32=n=>{v.setUint32(at,n,true);at+=4},u64=n=>{v.setBigUint64(at,BigInt(n),true);at+=8},f32=n=>{v.setFloat32(at,n,true);at+=4};
   request.set([67,71,74,49]);at=4;u64(1);for(let i=0;i<4;i++)u32(0);u64(0);for(let i=0;i<14;i++)u64(1);u32(3);for(let i=0;i<3;i++)f32(0);u32(186);for(let i=0;i<186*6;i++)f32(1);for(let i=0;i<256;i++){v.setUint16(at,65535,true);at+=2;}
   request[at++]=1;const index=8+16*(8+16*8),lights=new Set([index-1,index+1,index-16,index+16,index-256,index+256]);for(let i=0;i<4096;i++)u32(i===index?3:0);for(let i=0;i<4096;i++)u32(lights.has(i)?15<<8:0);for(let i=0;i<6;i++)request[at++]=0;if(at!==request.length)throw Error('GI fixture length '+at);
   const data=()=>{const payload=transport==='shared'?new SharedArrayBuffer(request.length):new ArrayBuffer(request.length);new Uint8Array(payload).set(request);return {type:'execute',id:'8589934593',transport,payload,control:transport==='shared'?new SharedArrayBuffer(4):undefined,outputLimit:524288}};
   let task=data(),reply=await call(task,transport==='transfer'?[task.payload]:[]);if(reply.type!=='result'||reply.id!==task.id)throw Error(JSON.stringify(reply));
   const bytes=new Uint8Array(reply.payload),out=new DataView(reply.payload),reads=Number(out.getBigUint64(92,true)),opaque=Number(out.getBigUint64(100,true)),faces=Number(out.getBigUint64(108,true));
   if(String.fromCharCode(...bytes.slice(0,4))!=='CGR1'||bytes[124]!==0||bytes[125]!==1||reads!==4102||opaque!==1||faces!==6||bytes.length!==213118)throw Error(JSON.stringify({reads,opaque,faces,responseBytes:bytes.length,status:bytes[124]}));
   const voxel=out.getUint32(126+index*4,true),surface=126+4096*4+index*48;if(voxel!==0xc0000020)throw Error('GI packed red radiance '+voxel);for(let f=0;f<6;f++)if(out.getUint32(surface+f*8,true)!==0x10ffffff||out.getUint32(surface+f*8+4,true)!==15)throw Error('GI face mismatch '+f);
   task=data();task.outputLimit=1;const bounded=await call(task,transport==='transfer'?[task.payload]:[]);if(bounded.type!=='error'||!bounded.error.includes('byte limit'))throw Error(JSON.stringify(bounded));
   task=data();new Uint8Array(task.payload)[0]=0;const invalid=await call(task,transport==='transfer'?[task.payload]:[]);if(invalid.type!=='error'||!invalid.error.includes('Magic'))throw Error(JSON.stringify(invalid));
   let cancellation=false;if(transport==='shared'){task=data();Atomics.store(new Int32Array(task.control),0,1);const cancelled=await call(task);if(cancelled.type!=='cancelled')throw Error(JSON.stringify(cancelled));cancellation=true;}
   results.push({transport,requestBytes:request.length,responseBytes:bytes.length,workerReads:reads,opaque,faces,packedRedRadiance:voxel,outputBound:true,invalidMagic:true,cancellation});
  }finally{worker.terminate()}
 }
 return results;
})()`));
assert.equal(result.length,2);console.log('BROWSER_GI_SOURCE_WORKERS_OK '+JSON.stringify(result));
}
