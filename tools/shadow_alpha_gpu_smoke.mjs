// Production public-cutout shader, immutable Rust-classified atlas, exact depth.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const directory='.local/distant-shadow-oracle';
const input={};for(const name of ['cutout.wgsl','camera.bin','cutout.pxp','alpha-flags.bin','alpha-rgba.bin'])input[name]=name.endsWith('wgsl')?await readFile(directory+'/'+name,'utf8'):Array.from(await readFile(directory+'/'+name));
async function run(input){
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU unavailable');
 const device=await adapter.requestDevice(),errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const B=GPUBufferUsage,T=GPUTextureUsage,N=128,results=[];
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b;};
 const shader=device.createShaderModule({code:input['cutout.wgsl']}),info=await shader.getCompilationInfo();if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages));
 const pipeline=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:shader,entryPoint:'vertex'},fragment:{module:shader,entryPoint:'fragment',targets:[]},primitive:{topology:'triangle-strip',cullMode:'none'},depthStencil:{format:'depth32float',depthWriteEnabled:true,depthCompare:'greater-equal'}});
 const raw=new Uint8Array(input['cutout.pxp']),header=new DataView(raw.buffer),words=new Uint32Array(raw.buffer.slice(36)),quadOffset=header.getUint32(24,true),quads=header.getUint32(28,true);
 const camera=buffer(144,B.UNIFORM|B.COPY_DST,new Uint8Array(input['camera.bin']));
 const object=new ArrayBuffer(256),f=new Float32Array(object),u=new Uint32Array(object);for(const base of [0,16])for(let i=0;i<4;i++)f[base+i*5]=1;f.set([32,16,16],12);f.set([-32,-16,-16],28);f.set([32,16,16,0],32);f.set([48,32,32,0],36);u.set([0,header.getUint32(8,true),header.getUint32(12,true),1],40);u.set([header.getUint32(32,true),quadOffset,quads,0],48);
 const objects=buffer(256,B.STORAGE|B.COPY_DST,object),data=buffer(words.byteLength,B.STORAGE|B.COPY_DST,words),indices=new Uint32Array(quads*2);for(let i=0;i<quads;i++)indices.set([0,quadOffset+4*i],2*i);
 const visible=buffer(indices.byteLength,B.STORAGE|B.COPY_DST,indices),materials=buffer(32,B.STORAGE|B.COPY_DST);
 const atlas=device.createTexture({size:[16,16,1],format:'rgba8unorm-srgb',usage:T.TEXTURE_BINDING|T.COPY_DST}),sampler=device.createSampler({magFilter:'nearest',minFilter:'nearest',mipmapFilter:'nearest',addressModeU:'repeat',addressModeV:'repeat'});
 const bind=device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:camera}},{binding:1,resource:{buffer:objects}},{binding:2,resource:{buffer:data}},{binding:3,resource:{buffer:visible}},{binding:4,resource:atlas.createView({dimension:'2d-array'})},{binding:6,resource:sampler},{binding:7,resource:{buffer:materials}}]});
 async function capture(flags,{material=0,layer=0}={}){
  const metadata=new Uint32Array([layer,flags,3,0x3f800000,9,2,0,0]);device.queue.writeBuffer(materials,0,metadata);
  const changed=words.slice();for(let i=0;i<quads;i++)changed[quadOffset+4*i+1]=(changed[quadOffset+4*i+1]&0xffff0000)|material;device.queue.writeBuffer(data,0,changed);
  const depth=device.createTexture({size:[N,N],format:'depth32float',usage:T.RENDER_ATTACHMENT|T.COPY_SRC}),read=buffer(N*N*4,B.MAP_READ|B.COPY_DST),e=device.createCommandEncoder();
  const p=e.beginRenderPass({colorAttachments:[],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(pipeline);p.setBindGroup(0,bind);p.draw(4,quads);p.end();e.copyTextureToBuffer({texture:depth,aspect:'depth-only'},{buffer:read,bytesPerRow:N*4},[N,N]);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const result=new Uint32Array(read.getMappedRange()).slice();read.unmap();read.destroy();depth.destroy();return result;
 }
 const bytes=new Uint8Array(input['alpha-rgba.bin']),flags=new Uint32Array(new Uint8Array(input['alpha-flags.bin']).buffer);
 for(let layer=0;layer<5;layer++){
  device.queue.writeTexture({texture:atlas},bytes.subarray(layer*1024,(layer+1)*1024),{bytesPerRow:64},[16,16]);
  const expected=await capture(0),actual=await capture(flags[layer]);let bad=0,covered=0;for(let i=0;i<actual.length;i++){bad+=Number(actual[i]!==expected[i]);covered+=Number(expected[i]!==0);}
  if(bad||((layer<2)!==(covered===0)))throw Error(JSON.stringify({layer,flags:flags[layer],bad,covered}));
  const control=layer===0?await capture(2):null;let falseOpaque=0;if(control){for(let i=0;i<control.length;i++)falseOpaque+=Number(control[i]!==expected[i]);if(falseOpaque<100)throw Error('alpha bypass negative control did not fail');}
  results.push({layer,flags:flags[layer],bad,covered,falseOpaque});
 }
 device.queue.writeTexture({texture:atlas},bytes.subarray(3072,4096),{bytesPerRow:64},[16,16]);
 for(const options of [{material:2},{material:1},{layer:9}]){const image=await capture(2,options);if(image.some(v=>v!==0))throw Error('invalid material/layer bypassed alpha bounds');results.push({invalid:options,covered:0});}
 // Same live atlas/bind/pipeline: invalidate facts before each queued upload.
 for(const layer of [0,4,3]){device.queue.writeTexture({texture:atlas},bytes.subarray(layer*1024,(layer+1)*1024),{bytesPerRow:64},[16,16]);const a=await capture(0),b=await capture(flags[layer]);if(a.some((v,i)=>v!==b[i]))throw Error('in-place/reset conservative fallback mismatch');results.push({mutation:layer,bad:0});}
 const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{shaderErrors:0,quads,drawsPerImage:1,results};
}
const result=await withBrowser({path:'/shadow-alpha-proof',width:128,height:128},async({evaluate})=>evaluate('('+run.toString()+')('+JSON.stringify(input)+')'));
assert.equal(result.results.length,11);await writeFile('.local/shadow-alpha-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log('DAWN_SHADOW_ALPHA_OK '+JSON.stringify(result));
