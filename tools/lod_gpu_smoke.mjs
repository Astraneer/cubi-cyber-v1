// Exact production Proxy WGSL, independent conventional-plane/A2C/depth oracle.
// Generate inputs with cargo run -p render --example lod_oracle_inputs first.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const directory='.local/lod-oracle';
const input={shader:await readFile(directory+'/proxy.wgsl','utf8'),payloads:{},cameras:{}};
for(const name of ['opaque','cutout','mixed','far'])input.payloads[name]=Array.from(await readFile(directory+'/'+name+'.pxp'));
for(const name of ['perspective','far'])input.cameras[name]=Array.from(await readFile(directory+'/'+name+'.camera'));
async function oracle(input){
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU unavailable');
 const device=await adapter.requestDevice(),errors=[];device.addEventListener('uncapturederror',event=>errors.push(event.error.message));device.pushErrorScope('validation');
 const B=GPUBufferUsage,T=GPUTextureUsage,N=96,results=[];
 const buffer=(size,usage,data)=>{const value=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(value,0,data);return value};
 const module=async code=>{const value=device.createShaderModule({code});const info=await value.getCompilationInfo();const bad=info.messages.filter(m=>m.type==='error');if(bad.length)throw Error(JSON.stringify(bad));return value};
 const production=await module(input.shader);
 const reference=await module('struct Camera{vp:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>};struct Object{model:mat4x4<f32>,inverse:mat4x4<f32>,minimum:vec4<f32>,maximum:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};@group(0)@binding(0)var<uniform>camera:Camera;@group(0)@binding(1)var<storage,read>objects:array<Object>;@group(0)@binding(4)var colors:texture_2d_array<f32>;@group(0)@binding(6)var texels:sampler;@group(0)@binding(7)var<storage,read>materials:array<vec4<u32>>;struct Out{@builtin(position)p:vec4<f32>,@location(0)@interpolate(flat)material:u32};@vertex fn vertex(@builtin(vertex_index)i:u32)->Out{let corners=array<vec2<f32>,6>(vec2(0.,0.),vec2(1.,0.),vec2(0.,1.),vec2(0.,1.),vec2(1.,0.),vec2(1.,1.));let uv=corners[i%6u];let plane=i/6u;var p=vec3(uv.x*16.,8.+f32(plane),(1.-uv.y)*16.);if camera.params.w==1u{p=vec3(uv*16.,16.);}var out:Out;out.p=camera.vp*objects[0].model*vec4(p,1.);out.material=3u+plane;return out;}@fragment fn fragment(input:Out)->@location(0)vec4<f32>{let color=textureSampleLevel(colors,texels,vec2(.5),i32(materials[input.material].x),0.);if color.a<.5{discard;}return color;}');
 const atlas=device.createTexture({size:[16,16,2],format:'rgba8unorm-srgb',usage:T.COPY_DST|T.TEXTURE_BINDING});
 const masks=device.createTexture({size:[16,16,2],format:'r8unorm',usage:T.COPY_DST|T.TEXTURE_BINDING});device.queue.writeTexture({texture:masks},new Uint8Array(16*16*2).fill(255),{bytesPerRow:16,rowsPerImage:16},[16,16,2]);
 const sampler=device.createSampler({magFilter:'nearest',minFilter:'nearest',mipmapFilter:'nearest',addressModeU:'repeat',addressModeV:'repeat'});
 const materials=new Uint32Array(5*4);materials[4*4]=1;const materialBuffer=buffer(materials.byteLength,B.STORAGE|B.COPY_DST,materials);
 for(const samples of [1,4]){
  const depthProbe=await module('@group(0)@binding(0)var source:DEPTH_TYPE;@group(0)@binding(1)var<storage,read_write>output:array<f32>;@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=96u||id.y>=96u{return;}for(var sample=0u;sample<COUNT;sample++){output[(id.y*96u+id.x)*COUNT+sample]=textureLoad(source,vec2<i32>(id.xy),i32(sample));}}'.replaceAll('DEPTH_TYPE',samples===1?'texture_depth_2d':'texture_depth_multisampled_2d').replaceAll('COUNT',samples+'u'));
  const probePipeline=await device.createComputePipelineAsync({layout:'auto',compute:{module:depthProbe,entryPoint:'read'}});
  const pipeline=async(shader,compare='greater-equal')=>device.createRenderPipelineAsync({layout:'auto',vertex:{module:shader,entryPoint:'vertex'},fragment:{module:shader,entryPoint:'fragment',targets:[{format:'rgba16float'}]},primitive:{cullMode:'back'},depthStencil:{format:'depth32float',depthCompare:compare,depthWriteEnabled:true},multisample:{count:samples,alphaToCoverageEnabled:samples>1}});
  const actualPipeline=await pipeline(production),referencePipeline=await pipeline(reference),strictPipeline=await pipeline(production,'greater');
  for(const [name,alpha]of [['opaque',255],['cutout',0],['cutout',128],['cutout',255],['far',255],['mixed',128]]){
   const packed=new Uint8Array(input.payloads[name]),header=new DataView(packed.buffer),words=new Uint32Array(packed.buffer.slice(24));
   const data=buffer(words.byteLength,B.STORAGE|B.COPY_DST,words),visible=buffer(4,B.STORAGE|B.COPY_DST,new Uint32Array([0]));
   const cameraBytes=new Uint8Array(input.cameras[name==='far'?'far':'perspective']),cameraWords=new Uint32Array(cameraBytes.buffer);if(name==='far')cameraWords[27]=1;
   const camera=buffer(112,B.UNIFORM|B.COPY_DST,cameraBytes);
   const object=new ArrayBuffer(256),f=new Float32Array(object),u=new Uint32Array(object);f.set([1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]);f.set(f.subarray(0,16),16);const translation=name==='far'?[0,0,-76]:[0,64,0];f.set([...translation,1],12);f.set(translation.map(v=>-v).concat(1),28);f.set([...translation,0],32);f.set(translation.map(v=>v+16).concat(0),36);u.set([0,header.getUint32(8,true),header.getUint32(12,true),1],40);f.set([0,0,0,1],44);
   const objects=buffer(256,B.STORAGE|B.COPY_DST,object),stats=buffer(16,B.STORAGE|B.COPY_DST|B.COPY_SRC);
   const texels=new Uint8Array(16*16*2*4);for(let i=0;i<16*16*2;i++)texels.set([255,0,255,i<256&&name==='mixed'?255:alpha],i*4);device.queue.writeTexture({texture:atlas},texels,{bytesPerRow:64,rowsPerImage:16},[16,16,2]);
   const entries=[{binding:0,resource:{buffer:camera}},{binding:1,resource:{buffer:objects}},{binding:2,resource:{buffer:data}},{binding:3,resource:{buffer:visible}},{binding:4,resource:atlas.createView({dimension:'2d-array'})},{binding:5,resource:masks.createView({dimension:'2d-array'})},{binding:6,resource:sampler},{binding:7,resource:{buffer:materialBuffer}},{binding:8,resource:{buffer:stats}}];
   const frame=async(pipeline,ref=false)=>{
    const color=device.createTexture({size:[N,N],format:'rgba16float',sampleCount:samples,usage:T.RENDER_ATTACHMENT|(samples===1?T.COPY_SRC:0)}),resolved=samples===1?color:device.createTexture({size:[N,N],format:'rgba16float',usage:T.RENDER_ATTACHMENT|T.COPY_SRC}),depth=device.createTexture({size:[N,N],format:'depth32float',sampleCount:samples,usage:T.RENDER_ATTACHMENT|T.TEXTURE_BINDING});
    const output=buffer(N*N*samples*4,B.STORAGE|B.COPY_SRC),colorBytes=N*N*8,depthBytes=N*N*samples*4,read=buffer(colorBytes+depthBytes+16,B.COPY_DST|B.MAP_READ);
    const bind=device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:ref?entries.filter(e=>[0,1,4,6,7].includes(e.binding)):entries});
    const probe=device.createBindGroup({layout:probePipeline.getBindGroupLayout(0),entries:[{binding:0,resource:depth.createView()},{binding:1,resource:{buffer:output}}]});
    const e=device.createCommandEncoder();e.clearBuffer(stats);let p=e.beginRenderPass({colorAttachments:[{view:color.createView(),resolveTarget:samples>1?resolved.createView():undefined,loadOp:'clear',clearValue:[.05,.08,.12,1],storeOp:'store'}],depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(pipeline);p.setBindGroup(0,bind);p.draw(ref?6:36);if(ref&&name==='mixed')p.draw(6,1,6);p.end();p=e.beginComputePass();p.setPipeline(probePipeline);p.setBindGroup(0,probe);p.dispatchWorkgroups(N/8,N/8);p.end();e.copyTextureToBuffer({texture:resolved},{buffer:read,bytesPerRow:N*8},[N,N]);e.copyBufferToBuffer(output,0,read,colorBytes,depthBytes);e.copyBufferToBuffer(stats,0,read,colorBytes+depthBytes,16);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const mapped=read.getMappedRange(),result={rgba:new Uint16Array(mapped,0,colorBytes/2).slice(),depth:new Float32Array(mapped,colorBytes,depthBytes/4).slice(),stats:Array.from(new Uint32Array(mapped,colorBytes+depthBytes,4))};read.unmap();for(const value of [color,resolved,depth,output,read])value.destroy();return result;
   };
   const expected=await frame(referencePipeline,true),actual=await frame(actualPipeline);let badDepth=0,greenMismatch=0,maxError=0,witnesses=0;
   for(let i=0;i<expected.depth.length;i++){const error=Math.abs(actual.depth[i]-expected.depth[i]);maxError=Math.max(maxError,error);if(error>1e-6)badDepth++;}
   for(let i=0;i<N*N;i++){if(expected.rgba[i*4+1]!==actual.rgba[i*4+1])greenMismatch++;if(expected.rgba[i*4+1]===0&&expected.rgba[i*4]>0)witnesses++;}
   let farNegative=0;if(name==='far'){const negative=await frame(strictPipeline);for(let i=0;i<N*N;i++)farNegative+=Number(negative.rgba[i*4+1]!==expected.rgba[i*4+1]);if(witnesses<1000||farNegative<1000)throw Error('Far-plane coverage gate lacks a negative control');}
   results.push({samples,name,alpha,badDepth,greenMismatch,maxError,witnesses,farNegative,stats:actual.stats});
   if(badDepth||greenMismatch)throw Error(JSON.stringify({message:'Proxy differs from independent conventional plane',results}));
  }
 }
 const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{shaderErrors:0,results};
}
const result=await withBrowser({path:'/lod-proxy-depth-oracle',width:96,height:96},async({evaluate})=>evaluate('('+oracle.toString()+')('+JSON.stringify(input)+')'));
assert.equal(result.results.length,12);await writeFile('.local/lod-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log('DAWN_LOD_DEPTH_OK '+JSON.stringify(result));
