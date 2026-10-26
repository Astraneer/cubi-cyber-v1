// Dawn executes the same bounded clear/restore shaders and validates model culls.
import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const common=await readFile('crates/render/src/shadow_footprint_common.wgsl','utf8');
const reduce=common+'\n'+await readFile('crates/render/src/shadow_footprint.wgsl','utf8');
const rust=await readFile('crates/render/src/shadows.rs','utf8');
const raw=name=>rust.match(new RegExp('const '+name+': &str = r#"([\\s\\S]*?)"#;'))[1];
const rectangle=common+'\n'+raw('SPARSE_RECT_SHADER');
const restore=rectangle+'\n'+raw('RESTORE_SHADER').replace('binding(0)','binding(2)');
const helper=(await readFile('crates/render/src/shadow_footprint.rs','utf8')).match(/const MODEL: &str = r#"([\s\S]*?)"#;/)[1];
const models=await Promise.all(['entity_cull','block_models_cull'].map(async name=>common+'\n'+helper+'\n'+await readFile('crates/render/src/'+name+'.wgsl','utf8')));
const result=await withBrowser({path:'/shadow-footprint-oracle',width:64,height:64},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('Missing WebGPU');const device=await adapter.requestDevice();const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const codes=${JSON.stringify([reduce,rectangle,restore,...models])};const modules=[];
 for(const code of codes){const module=device.createShaderModule({code});const info=await module.getCompilationInfo();const bad=info.messages.filter(m=>m.type==='error');if(bad.length)throw Error(JSON.stringify(bad.map(m=>({line:m.lineNum,message:m.message}))));modules.push(module);}
 const B=GPUBufferUsage,T=GPUTextureUsage,S=GPUShaderStage;
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b;};
 const cameraData=new ArrayBuffer(96),cf=new Float32Array(cameraData),cu=new Uint32Array(cameraData);cf.set([1/32,0,0,0,0,1/32,0,0,0,0,1/2048,0,0,0,.5,1]);cf.set([64,64,0,0],16);cu[20]=1;
 const camera=buffer(96,B.UNIFORM|B.COPY_DST,cameraData),objects=buffer(128,B.STORAGE|B.COPY_DST),footprint=buffer(256,B.STORAGE|B.COPY_SRC);
 const objectData=new ArrayBuffer(128),of=new Float32Array(objectData),ou=new Uint32Array(objectData);of.set([1,0,0,0,0,1,0,0,0,0,1,0,0,0,0,1]);of.set([-4,-2,-3],16);of.set([4,2,3],20);ou[25]=1;ou[30]=1;ou[31]=1;device.queue.writeBuffer(objects,0,objectData);
 const layout=device.createBindGroupLayout({entries:[{binding:0,visibility:S.COMPUTE,buffer:{type:'uniform'}},{binding:1,visibility:S.COMPUTE,buffer:{type:'read-only-storage'}},{binding:2,visibility:S.COMPUTE,buffer:{type:'storage'}}]});const pl=device.createPipelineLayout({bindGroupLayouts:[layout]});
 const pipelines=[];for(const entryPoint of ['reset_bounds','reduce_terrain','finish_terrain','finish_combined','finish_combined_full'])pipelines.push(await device.createComputePipelineAsync({layout:pl,compute:{module:modules[0],entryPoint}}));
 const bind=device.createBindGroup({layout,entries:[[0,camera],[1,objects],[2,footprint]].map(([binding,buffer])=>({binding,resource:{buffer}}))});
 for(const module of modules.slice(3))for(const entryPoint of ['reset','cull'])await device.createComputePipelineAsync({layout:'auto',compute:{module,entryPoint}});
 const depth=(usage)=>device.createTexture({size:[64,64],format:'depth32float',usage});const source=depth(T.RENDER_ATTACHMENT|T.COPY_SRC|T.TEXTURE_BINDING),target=depth(T.RENDER_ATTACHMENT|T.COPY_SRC);
 const state={format:'depth32float',depthCompare:'always',depthWriteEnabled:true};
 const clear=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:modules[1],entryPoint:'vertex'},depthStencil:state});
 const cb=device.createBindGroup({layout:clear.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:camera}},{binding:1,resource:{buffer:footprint}}]});
 const restore=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:modules[2],entryPoint:'restore_vertex'},fragment:{module:modules[2],entryPoint:'restore',targets:[]},depthStencil:state});
 const rb=device.createBindGroup({layout:restore.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:camera}},{binding:1,resource:{buffer:footprint}},{binding:2,resource:source.createView()}]});
 const read=buffer(256*64*2+256,B.MAP_READ|B.COPY_DST);const e=device.createCommandEncoder();let p=e.beginComputePass();p.setBindGroup(0,bind);for(const pipeline of pipelines.slice(0,4)){p.setPipeline(pipeline);p.dispatchWorkgroups(1);}p.end();
 const pass=(texture)=>e.beginRenderPass({colorAttachments:[],depthStencilAttachment:{view:texture.createView(),depthClearValue:.9,depthLoadOp:'clear',depthStoreOp:'store'}});
 p=pass(source);p.setPipeline(clear);p.setBindGroup(0,cb);p.draw(6);p.end();p=pass(target);p.setPipeline(restore);p.setBindGroup(0,rb);p.draw(6);p.end();
 for(const [i,texture] of [source,target].entries())e.copyTextureToBuffer({texture,aspect:'depth-only'},{buffer:read,offset:i*256*64,bytesPerRow:256},[64,64]);e.copyBufferToBuffer(footprint,0,read,256*64*2,256);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const bytes=read.getMappedRange();const first=new Float32Array(bytes,0,4096),second=new Float32Array(bytes,256*64,4096),record=new Uint32Array(bytes,256*64*2,64);let zeros=0,untouched=0;
 for(let i=0;i<4096;i++){if(first[i]!==second[i])throw Error('Restore differs at '+i);if(first[i]===0)zeros++;else if(Math.abs(first[i]-.9)<1e-6)untouched++;else throw Error('Unexpected depth '+first[i]);}
 if(zeros!==252||untouched!==3844||record[24]!==252||record[25]!==252)throw Error(JSON.stringify({zeros,untouched,metrics:Array.from(record.slice(24,28))}));read.unmap();await device.queue.onSubmittedWorkDone();const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{pipelines:11,shaderErrors:0,exactZeroPixels:zeros,untouchedPixels:untouched,restoreBitwise:true};
})()`));
assert.equal(result.restoreBitwise,true);await writeFile('.local/shadow-footprint-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log('DAWN_SHADOW_FOOTPRINT_OK '+JSON.stringify(result));
