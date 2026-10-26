// Exact production cull + sampler-free/partial shadow buckets. The independent
// reference keeps the original ordinary-quad VS and draws every public quad.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const directory='.local/distant-shadow-oracle',input={};
for(const name of ['cull.wgsl','cutout.wgsl','cutout-sampling-reference.wgsl','camera.bin','cutout.pxp','cutout-materials.pxp','alpha-flags.bin','alpha-rgba.bin'])input[name]=name.endsWith('wgsl')?await readFile(directory+'/'+name,'utf8'):Array.from(await readFile(directory+'/'+name));
async function run(input){
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU unavailable');const device=await adapter.requestDevice(),errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const B=GPUBufferUsage,T=GPUTextureUsage,N=128,results=[];
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b;};
 const module=async code=>{const s=device.createShaderModule({code});const info=await s.getCompilationInfo();if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages));return s;};
 const [shader,legacy,cullShader]=await Promise.all(['cutout.wgsl','cutout-sampling-reference.wgsl','cull.wgsl'].map(name=>module(input[name])));
 const pipeline=(module,vertex='vertex',fragment=true,layout='auto')=>device.createRenderPipelineAsync({layout,vertex:{module,entryPoint:vertex},fragment:fragment?{module,entryPoint:'fragment',targets:[]}:undefined,primitive:{topology:'triangle-strip',cullMode:'none'},depthStencil:{format:'depth32float',depthWriteEnabled:true,depthCompare:'greater-equal'}});
 const V=GPUShaderStage.VERTEX,F=GPUShaderStage.FRAGMENT,cutoutLayout=device.createBindGroupLayout({entries:[{binding:0,visibility:V,buffer:{type:'uniform'}},...[1,2,3].map(binding=>({binding,visibility:V,buffer:{type:'read-only-storage'}})),{binding:4,visibility:F,texture:{sampleType:'float',viewDimension:'2d-array'}},{binding:6,visibility:F,sampler:{type:'filtering'}},{binding:7,visibility:F,buffer:{type:'read-only-storage'}}]}),cutoutPipelineLayout=device.createPipelineLayout({bindGroupLayouts:[cutoutLayout]});
 const partial=await pipeline(shader,'partial_vertex',true,cutoutPipelineLayout),passAll=await pipeline(shader,'vertex',false,cutoutPipelineLayout),reference=await pipeline(legacy);
 const reset=await device.createComputePipelineAsync({layout:'auto',compute:{module:cullShader,entryPoint:'reset'}}),cull=await device.createComputePipelineAsync({layout:'auto',compute:{module:cullShader,entryPoint:'cull'}});
 const raw=new Uint8Array(input['cutout.pxp']),header=new DataView(raw.buffer);let words=new Uint32Array(raw.buffer.slice(36)),quadOffset=header.getUint32(24,true),quads=header.getUint32(28,true);
 const uniformQuads=quads;
 const cameraBytes=new Uint8Array(input['camera.bin']),cameraWords=new Uint32Array(cameraBytes.buffer);cameraWords[24]=0;cameraWords[25]=1;cameraWords[27]=4; // Actual atlas2layers, footprint collection0.
 const camera=buffer(144,B.UNIFORM|B.COPY_DST,cameraBytes),object=new ArrayBuffer(256),f=new Float32Array(object),u=new Uint32Array(object);
 for(const base of [0,16])for(let i=0;i<4;i++)f[base+i*5]=1;f.set([32,16,16],12);f.set([-32,-16,-16],28);f.set([32,16,16,0],32);f.set([48,32,32,0],36);u.set([0,header.getUint32(8,true),header.getUint32(12,true),1],40);u.set([header.getUint32(32,true),quadOffset,quads,0],48);
 const objects=buffer(256,B.STORAGE|B.COPY_DST,object),data=buffer(65536*4,B.STORAGE|B.COPY_DST,words),visible=buffer(512+131072,B.STORAGE),indirect=buffer(64,B.STORAGE|B.INDIRECT|B.COPY_SRC),tiles=buffer(64,B.STORAGE),footprint=buffer(256,B.STORAGE),materials=buffer(32,B.STORAGE|B.COPY_DST);
 const indices=new Uint32Array(quads*2);for(let i=0;i<quads;i++)indices.set([0,quadOffset+4*i],2*i);const originalVisible=buffer(16384*8,B.STORAGE|B.COPY_DST,indices);
 const atlas=device.createTexture({size:[16,16,2],format:'rgba8unorm-srgb',usage:T.TEXTURE_BINDING|T.COPY_DST}),sampler=device.createSampler({magFilter:'nearest',minFilter:'nearest',mipmapFilter:'nearest',addressModeU:'repeat',addressModeV:'repeat'});
 const e=(binding,b,offset=0,size=undefined)=>({binding,resource:{buffer:b,offset,size}}),bind=(p,entries)=>device.createBindGroup({layout:p.getBindGroupLayout(0),entries});
 const baseEntries=[e(0,camera),e(1,objects),e(2,data),e(3,visible,512,131072),{binding:4,resource:atlas.createView({dimension:'2d-array'})},{binding:6,resource:sampler},e(7,materials)];
 const actualBind=bind(partial,baseEntries),referenceBind=bind(reference,baseEntries.map(entry=>entry.binding===3?e(3,originalVisible):entry));
 const resetBind=bind(reset,[e(4,indirect)]),cullBind=bind(cull,[e(0,camera),e(1,tiles),e(2,objects),e(3,visible),e(4,indirect),e(5,footprint),e(6,data),e(7,materials)]);
 async function capture(flags,{material=0,layer=0,mixedMaterials=false}={},old=false){
  device.queue.writeBuffer(materials,0,new Uint32Array([layer,flags,3,0x3f800000,mixedMaterials?1:9,mixedMaterials?0:2,0,0]));const changed=words.slice();for(let i=0;i<quads&&!mixedMaterials;i++)changed[quadOffset+4*i+1]=(changed[quadOffset+4*i+1]&0xffff0000)|material;device.queue.writeBuffer(data,0,changed);
  const depth=device.createTexture({size:[N,N],format:'depth32float',usage:T.RENDER_ATTACHMENT|T.COPY_SRC}),read=buffer(N*N*4+64,B.MAP_READ|B.COPY_DST),encoder=device.createCommandEncoder();
  let p=encoder.beginComputePass();p.setPipeline(reset);p.setBindGroup(0,resetBind);p.dispatchWorkgroups(1);if(!old){p.setPipeline(cull);p.setBindGroup(0,cullBind);p.dispatchWorkgroups(1);}p.end();
  p=encoder.beginRenderPass({colorAttachments:[],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});
  if(old){p.setPipeline(reference);p.setBindGroup(0,referenceBind);p.draw(4,quads);}else{p.setBindGroup(0,actualBind);p.setPipeline(passAll);p.drawIndirect(indirect,48);p.setPipeline(partial);p.drawIndirect(indirect,32);}p.end();
  encoder.copyTextureToBuffer({texture:depth,aspect:'depth-only'},{buffer:read,bytesPerRow:N*4},[N,N]);encoder.copyBufferToBuffer(indirect,0,read,N*N*4,64);device.queue.submit([encoder.finish()]);await read.mapAsync(GPUMapMode.READ);
  const memory=read.getMappedRange(),image=new Uint32Array(memory,0,N*N).slice(),commands=Array.from(new Uint32Array(memory,N*N*4,16));read.unmap();read.destroy();depth.destroy();return{image,commands};
 }
 const bytes=new Uint8Array(input['alpha-rgba.bin']),flags=new Uint32Array(new Uint8Array(input['alpha-flags.bin']).buffer);
 const check=(actual,expected)=>{let bad=0,covered=0;for(let i=0;i<actual.length;i++){bad+=Number(actual[i]!==expected[i]);covered+=Number(expected[i]!==0);}if(bad)throw Error(JSON.stringify({message:'bucket VS/depth differs from original sampling',bad,covered}));return covered;};
 for(let layer=0;layer<5;layer++){
  device.queue.writeTexture({texture:atlas},bytes.subarray(layer*1024,(layer+1)*1024),{bytesPerRow:64},[16,16]);const expected=await capture(0,{},true),actual=await capture(flags[layer]);const covered=check(actual.image,expected.image),mixed=actual.commands[9],pass=actual.commands[13];
  if(mixed+pass>quads||mixed!==(flags[layer]===0?quads:0)||pass!==((flags[layer]&2)!==0?quads:0)||((layer<2)!==(covered===0)))throw Error(JSON.stringify({layer,flags:flags[layer],mixed,pass,covered}));
  let falseOpaque=0;if(layer===0){const control=await capture(2);for(let i=0;i<control.image.length;i++)falseOpaque+=Number(control.image[i]!==expected.image[i]);if(falseOpaque<100)throw Error('false PASS negative control lacks witnesses');}
  results.push({layer,flags:flags[layer],bad:0,covered,mixed,pass,falseOpaque});
 }
 device.queue.writeTexture({texture:atlas},bytes.subarray(3072,4096),{bytesPerRow:64},[16,16]);
 for(const options of [{material:2},{material:1},{layer:9}]){const a=await capture(2,options),b=await capture(0,options,true);check(a.image,b.image);if(a.image.some(v=>v!==0)||a.commands[9]+a.commands[13]!==0)throw Error('invalid material/layer became opaque');results.push({invalid:options,covered:0});}
 const conflict=await capture(6);if(conflict.image.some(v=>v!==0)||conflict.commands[9]+conflict.commands[13]!==0)throw Error('FAIL precedence did not reject contradictory flags');results.push({conflictingFlags:6,covered:0});
 for(const layer of [0,4,3]){device.queue.writeTexture({texture:atlas},bytes.subarray(layer*1024,(layer+1)*1024),{bytesPerRow:64},[16,16]);const a=await capture(0),b=await capture(0,{},true);check(a.image,b.image);if(a.commands[9]!==quads||a.commands[13]!==0)throw Error('in-place invalidation resurrected old PASS list');results.push({mutation:layer,bad:0,mixed:quads,pass:0});}
 // Coexisting PASS and MIXED materials from an actual public-cell fixture.
 const mixedRaw=new Uint8Array(input['cutout-materials.pxp']),mixedHeader=new DataView(mixedRaw.buffer);
 words=new Uint32Array(mixedRaw.buffer.slice(36));quadOffset=mixedHeader.getUint32(24,true);quads=mixedHeader.getUint32(28,true);
 const pairs=new Uint32Array(quads*2);for(let i=0;i<quads;i++)pairs.set([0,quadOffset+4*i],2*i);device.queue.writeBuffer(originalVisible,0,pairs);
 u.set([0,mixedHeader.getUint32(8,true),mixedHeader.getUint32(12,true),1],40);u.set([mixedHeader.getUint32(32,true),quadOffset,quads,0],48);device.queue.writeBuffer(objects,0,object);
 device.queue.writeTexture({texture:atlas},bytes.subarray(3072,4096),{bytesPerRow:64},[16,16]);device.queue.writeTexture({texture:atlas,origin:[0,0,1]},bytes.subarray(4096,5120),{bytesPerRow:64},[16,16]);
 const mixedActual=await capture(2,{mixedMaterials:true}),mixedReference=await capture(0,{mixedMaterials:true},true);const mixedCovered=check(mixedActual.image,mixedReference.image),partialCount=mixedActual.commands[9],passCount=mixedActual.commands[13];
 if(!partialCount||!passCount||partialCount+passCount!==quads)throw Error('public mixed fixture did not exercise both buckets');results.push({coexistingMaterials:true,bad:0,covered:mixedCovered,quads,mixed:partialCount,pass:passCount});
 const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{shaderErrors:0,computeStorage:7,uniformQuads,drawGroups:2,knownPassFragment:false,results};
}
const result=await withBrowser({path:'/shadow-alpha-proof',width:128,height:128},async({evaluate})=>evaluate('('+run.toString()+')('+JSON.stringify(input)+')'));
assert.equal(result.results.length,13);await writeFile('.local/shadow-alpha-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log('DAWN_SHADOW_ALPHA_OK '+JSON.stringify(result));
