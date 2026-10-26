// Run `cargo run --locked -p render --example model_shaders` first. These are
// the exact constructor sources, not a separate implementation of illumination.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const sourceTasks = [];
for (const kind of ['entity', 'block']) {
  for (const gi of [false, true]) {
    for (const ambient of [false, true]) {
      const name = kind + (gi ? '-gi' : '-no-gi') + (ambient ? '-mrt' : '');
      sourceTasks.push(readFile(`.local/model-shaders/${name}.wgsl`, 'utf8').then(code => [name, code]));
    }
  }
}
const sources = Object.fromEntries(await Promise.all(sourceTasks));
const result=await withBrowser({path:'/model-gi-receiver-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU unavailable');const device=await adapter.requestDevice({requiredLimits:{maxStorageBuffersPerShaderStage:8}}),errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));
 const source=${JSON.stringify(sources)},B=GPUBufferUsage,S=GPUShaderStage,buffer=(data,usage)=>{const b=device.createBuffer({size:data.byteLength,usage:usage|B.COPY_DST});device.queue.writeBuffer(b,0,data);return b};
 const camera=new ArrayBuffer(96);new Float32Array(camera).set([.8,0,0,0,0,.8,0,0,0,0,.05,0,-6.4,-6.8,.1,1]);new Uint32Array(camera).set([1,1,0,0],16);const cb=buffer(camera,B.UNIFORM);
 const ambient=new Float32Array(20);ambient[0]=ambient[4]=ambient[8]=10;ambient.set([20,20,20,0],12);ambient[17]=1;const atmosphere=buffer(ambient,B.STORAGE);
 const params=new ArrayBuffer(96),pf=new Float32Array(params),pu=new Uint32Array(params);pf.set([6.5,6.5,6.5,4]);pu.set([2,2,2,0],4);pu.set([1,1,1,0],12);const gp=buffer(params,B.UNIFORM);
 const field=new ArrayBuffer(8*128),ff=new Float32Array(field),fu=new Uint32Array(field);for(let i=0;i<8;i++){const at=i*32;ff.set([6.5+4*(i%2),6.5+4*Math.floor(i/4),6.5+4*(Math.floor(i/2)%2),1],at);ff[at+4]=Math.PI*.25/.2820947918;ff[at+8]=Math.PI*.5/.2820947918;ff[at+12]=Math.PI*.75/.2820947918;ff.fill(32,at+20,at+28);fu.set([1,1,1,32],at+28);}const probes=buffer(field,B.STORAGE),world=buffer(new Uint32Array(4096).fill(0x80000000),B.STORAGE);
 const albedo=device.createTexture({size:[1,1,1],format:'rgba8unorm-srgb',usage:GPUTextureUsage.COPY_DST|GPUTextureUsage.TEXTURE_BINDING});device.queue.writeTexture({texture:albedo},new Uint8Array([128,192,64,255]),{bytesPerRow:4},[1,1,1]);const atlas=albedo.createView({dimension:'2d-array'}),sampler=device.createSampler({magFilter:'nearest',minFilter:'nearest',mipmapFilter:'nearest',addressModeU:'repeat',addressModeV:'repeat'});
 const baselines={};const points=[[-.5,0,0],[.5,0,0],[-.5,1,0],[-.5,1,0],[.5,0,0],[.5,1,0]],results=[];
 const half=h=>{const sign=h&32768?-1:1,e=h>>10&31,m=h&1023;return e===0?sign*m*2**-24:sign*(1+m/1024)*2**(e-15)};
 for(const kind of ['entity','block'])for(const ambientMrt of [false,true]){
  const entity=kind==='entity',instance=new ArrayBuffer(64),iv=new Float32Array(instance),iu=new Uint32Array(instance);iv.set([8,8,8]);if(entity){iu.set([6,4096|32,0,1],8);iv[14]=1;}else{iu[4]=5;iu[10]=0xff40ff80;}
  const instances=buffer(instance,B.STORAGE),visible=buffer(new Uint32Array(entity?8:17),B.STORAGE),lights=buffer(new Uint32Array([0]),B.STORAGE);
  const vertices=new ArrayBuffer(6*64),vf=new Float32Array(vertices),vu=new Uint32Array(vertices);for(let i=0;i<6;i++){const at=i*16;vf.set(points[i],at);if(entity){vf.set([0,0,1],at+3);vf.set([i%2,i>1?1:0],at+6);vu[at+11]=6;}else{vf.set([0,0,1],at+4);vf.set([i%2,i>1?1:0],at+8);vu[at+11]=5;vu[at+15]=1;}}
  const mesh=buffer(vertices,entity?B.VERTEX:B.STORAGE),pipelines={},binds={};
  for(const gi of [false,true]){
   const module=device.createShaderModule({code:source[kind+(gi?'-gi':'-no-gi')+(ambientMrt?'-mrt':'')]});const info=await module.getCompilationInfo();if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages.map(m=>({line:m.lineNum,type:m.type,text:m.message}))));
   const be=(binding,uniform,fragment=false)=>({binding,visibility:fragment?S.FRAGMENT:S.VERTEX,buffer:{type:uniform?'uniform':'read-only-storage'}}),tex=binding=>({binding,visibility:S.FRAGMENT,texture:{sampleType:'float',viewDimension:'2d-array'}});
   const entries=[be(0,true),be(1,false),be(2,false),be(3,false),tex(4),{binding:5,visibility:S.FRAGMENT,sampler:{type:'filtering'}},be(20,false,true)];if(entity)entries.push(tex(6),tex(7));if(gi)entries.push(be(13,true,true),be(14,false,true),be(15,false,true));
   const layout=device.createBindGroupLayout({entries}),pl=device.createPipelineLayout({bindGroupLayouts:[layout]}),attributes=entity?[{shaderLocation:0,offset:0,format:'float32x3'},{shaderLocation:1,offset:12,format:'float32x3'},{shaderLocation:2,offset:24,format:'float32x2'},{shaderLocation:3,offset:32,format:'float32x3'},{shaderLocation:4,offset:44,format:'uint32'},{shaderLocation:5,offset:48,format:'uint32'},{shaderLocation:6,offset:52,format:'uint32'},{shaderLocation:7,offset:56,format:'float32'},{shaderLocation:8,offset:60,format:'uint32'}]:[];
   pipelines[gi]=await device.createRenderPipelineAsync({layout:pl,vertex:{module,entryPoint:'vertex',buffers:entity?[{arrayStride:64,attributes}]:[]},primitive:{cullMode:'none'},depthStencil:{format:'depth32float',depthWriteEnabled:true,depthCompare:'greater-equal'},multisample:{count:4,alphaToCoverageEnabled:true},fragment:{module,entryPoint:'fragment',targets:Array.from({length:ambientMrt?2:1},()=>({format:'rgba16float'}))}});
   const binding=[{binding:0,resource:{buffer:cb}},{binding:1,resource:{buffer:instances}},{binding:2,resource:{buffer:entity?visible:mesh}},{binding:3,resource:{buffer:entity?lights:visible}},{binding:4,resource:atlas},{binding:5,resource:sampler},{binding:20,resource:{buffer:atmosphere}}];if(entity)binding.push({binding:6,resource:atlas},{binding:7,resource:atlas});if(gi)binding.push({binding:13,resource:{buffer:gp}},{binding:14,resource:{buffer:probes}},{binding:15,resource:{buffer:world}});binds[gi]=device.createBindGroup({layout,entries:binding});
  }
  const texture=(format,samples)=>device.createTexture({size:[64,64],format,sampleCount:samples,usage:GPUTextureUsage.RENDER_ATTACHMENT|(samples===1?GPUTextureUsage.COPY_SRC:0)}),msaa=texture('rgba16float',4),resolved=texture('rgba16float',1),depth=texture('depth32float',4),ambientMsaa=ambientMrt?texture('rgba16float',4):null,ambientResolved=ambientMrt?texture('rgba16float',1):null;
  const capture=async(gi,full,known)=>{
   device.queue.writeBuffer(world,0,new Uint32Array(4096).fill(known?0x80000000:0));if(entity)device.queue.writeBuffer(lights,0,new Uint32Array([full?0x0f0f0f00:0]));else{iu[7]=full?0x0f0f0f00:0;device.queue.writeBuffer(instances,0,instance);}
   const read=device.createBuffer({size:64*64*8*(ambientMrt?2:1),usage:B.COPY_DST|B.MAP_READ}),e=device.createCommandEncoder(),p=e.beginRenderPass({colorAttachments:[{view:msaa.createView(),resolveTarget:resolved.createView(),clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'},...(ambientMrt?[{view:ambientMsaa.createView(),resolveTarget:ambientResolved.createView(),clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'}]:[])],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(pipelines[gi]);p.setBindGroup(0,binds[gi]);if(entity)p.setVertexBuffer(0,mesh);p.draw(6,1);p.end();e.copyTextureToBuffer({texture:resolved},{buffer:read,bytesPerRow:512},[64,64]);if(ambientMrt)e.copyTextureToBuffer({texture:ambientResolved},{buffer:read,offset:64*64*8,bytesPerRow:512},[64,64]);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const raw=read.getMappedRange().slice(0),u=new Uint16Array(raw),image=Array.from({length:4096},(_,i)=>[0,1,2,3].map(c=>half(u[i*4+c])));read.unmap();read.destroy();return {raw:[...new Uint8Array(raw,0,64*64*8)],image,delta:ambientMrt?Array.from({length:4096},(_,i)=>[0,1,2,3].map(c=>half(u[(4096+i)*4+c]))):null};
  };
  const dark=await capture(false,false,true),reference=await capture(false,true,true),actual=await capture(true,false,true),unknown=await capture(true,false,false),restored=await capture(true,false,true),baseline=await capture(false,true,false);let pixels=0,error=0;
  for(let i=0;i<4096;i++){for(let c=0;c<3;c++)if(dark.image[i][c]!==0||unknown.image[i][c]!==0)throw Error(kind+' hidden sky or Unknown GI leak');if(reference.image[i][3]<.999)continue;pixels++;for(let c=0;c<3;c++)error=Math.max(error,Math.abs(actual.image[i][c]-reference.image[i][c]*[.25,.5,.75][c]));}
  if(pixels<50||error>.001||JSON.stringify(actual.raw)!==JSON.stringify(restored.raw)||JSON.stringify(reference.raw)!==JSON.stringify(baseline.raw))throw Error(JSON.stringify({kind,pixels,error}));let deltaError=0;if(ambientMrt){for(let i=0;i<4096;i++)for(let c=0;c<3;c++)deltaError=Math.max(deltaError,Math.abs(actual.delta[i][c]-actual.image[i][c]));if(deltaError>.00002||JSON.stringify(actual.raw)!==JSON.stringify(baselines[kind]))throw Error('MRT changed first output or removable energy '+kind+' '+deltaError);}else baselines[kind]=actual.raw;results.push({kind,ambientMrt,ambientDeltaError:deltaError,pixels,maxHDRerror:error,sky0Dark:true,UnknownDark:true,hotSourceRestoreBitwise:true,NoGiBitwiseAfterSourceChanges:true,pipelines:2,fragmentStorageBuffers:3,vertexStorageBuffers:3});
 }
 if(errors.length)throw Error(JSON.stringify(errors));device.destroy();return {portableStorageLimit:8,results};
})()`));
assert.equal(result.results.length,4);console.log('DAWN_MODEL_GI_RECEIVER_OK '+JSON.stringify(result));
