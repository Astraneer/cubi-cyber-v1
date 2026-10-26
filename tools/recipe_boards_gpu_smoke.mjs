// Exact prepared world-card shader variants. Synthetic atlas colours give an
// independent depth/image oracle; the CPU artwork test verifies recipe content.
import assert from 'node:assert/strict';
import {readFile,readdir,writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const root=process.argv[2]??'.local/recipe-board-shaders';
const sources=Object.fromEntries(await Promise.all((await readdir(root)).filter(n=>n.endsWith('.wgsl')).map(async n=>[n,await readFile(root+'/'+n,'utf8')])));
async function run(sources){
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU adapter missing');const device=await adapter.requestDevice();
 const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));device.pushErrorScope('validation');
 const B=GPUBufferUsage,T=GPUTextureUsage,S=GPUShaderStage;
 const buffer=(size,usage,data)=>{const b=device.createBuffer({size,usage});if(data)device.queue.writeBuffer(b,0,data);return b;};
 const matrices=new Float32Array(20);for(let i=0;i<4;i++)matrices[i*5]=1;
 const camera=buffer(80,B.UNIFORM|B.COPY_DST,matrices),records=new ArrayBuffer(128),f=new Float32Array(records),u=new Uint32Array(records),boards=buffer(128,B.STORAGE|B.COPY_DST);
 const texture=device.createTexture({size:[1,1,2],format:'rgba8unorm-srgb',usage:T.TEXTURE_BINDING|T.COPY_DST});device.queue.writeTexture({texture},new Uint8Array([255,0,0,255,0,255,0,255]),{bytesPerRow:4,rowsPerImage:1},[1,1,2]);const sampler=device.createSampler({magFilter:'nearest',minFilter:'nearest',mipmapFilter:'nearest'});
 const layout=device.createBindGroupLayout({entries:[{binding:0,visibility:S.VERTEX,buffer:{type:'uniform'}},{binding:1,visibility:S.VERTEX,buffer:{type:'read-only-storage'}},{binding:2,visibility:S.FRAGMENT,texture:{viewDimension:'2d-array',sampleType:'float'}},{binding:3,visibility:S.FRAGMENT,sampler:{type:'filtering'}}]});
 const bind=device.createBindGroup({layout,entries:[{binding:0,resource:{buffer:camera}},{binding:1,resource:{buffer:boards}},{binding:2,resource:texture.createView({dimension:'2d-array'})},{binding:3,resource:sampler}]});
 let compiled=0;const modules={};for(const[name,code]of Object.entries(sources)){const module=device.createShaderModule({code});const bad=(await module.getCompilationInfo()).messages.filter(m=>m.type==='error');if(bad.length)throw Error(name+JSON.stringify(bad));modules[name]=module;compiled++;}
 const results=[];
 function row(index,layer,z,size){const start=index*16;f.set([0,0,z,0,1,0,0,size,0,1,0,size],start);u.set([layer,0x0f0f0f00,1,0],start+12);}
 for(const samples of[1,4])for(const mrt of[false,true]){
  const module=modules['board-000'+Number(mrt)+'.wgsl'];const pipeline=await device.createRenderPipelineAsync({layout:device.createPipelineLayout({bindGroupLayouts:[layout]}),vertex:{module,entryPoint:'vertex'},primitive:{cullMode:'back'},multisample:{count:samples},depthStencil:{format:'depth32float',depthWriteEnabled:true,depthCompare:'greater-equal'},fragment:{module,entryPoint:'fragment',targets:Array.from({length:mrt?2:1},()=>({format:'rgba16float'}))}});
  const tex=(format,count,usage)=>device.createTexture({size:[32,32],format,sampleCount:count,usage});const output=tex('rgba16float',1,T.RENDER_ATTACHMENT|T.COPY_SRC),msaa=tex('rgba16float',samples,T.RENDER_ATTACHMENT),ambient=tex('rgba16float',samples,T.RENDER_ATTACHMENT),depth=tex('depth32float',samples,T.RENDER_ATTACHMENT);
  async function capture(front){f.fill(0);row(0,front?1:0,front?.75:.5,front?.5:1.5);if(front)row(1,0,.5,1.5);device.queue.writeBuffer(boards,0,records);const e=device.createCommandEncoder(),attachments=[{view:(samples>1?msaa:output).createView(),resolveTarget:samples>1?output.createView():undefined,loadOp:'clear',storeOp:'store',clearValue:[0,0,0,0]}];if(mrt)attachments.push({view:ambient.createView(),loadOp:'clear',storeOp:'store',clearValue:[0,0,0,0]});const p=e.beginRenderPass({colorAttachments:attachments,depthStencilAttachment:{view:depth.createView(),depthClearValue:0,depthLoadOp:'clear',depthStoreOp:'store'}});p.setPipeline(pipeline);p.setBindGroup(0,bind);p.draw(6,2);p.end();const read=buffer(8192,B.MAP_READ|B.COPY_DST);e.copyTextureToBuffer({texture:output},{buffer:read,bytesPerRow:256},[32,32]);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const result=new Uint16Array(read.getMappedRange().slice(0));read.unmap();read.destroy();return result;}
  const red=await capture(false),occluded=await capture(true);let green=0,redCount=0;for(let y=0;y<32;y++)for(let x=0;x<32;x++){const offset=4*(y*32+x);if(x>=12&&x<20&&y>=12&&y<20){const want=[0,0x3c00,0,0x3c00];if(want.some((v,c)=>occluded[offset+c]!==v))throw Error(JSON.stringify({samples,mrt,x,y,actual:[...occluded.slice(offset,offset+4)]}));green++;}if(red[offset]===0x3c00)redCount++;}if(redCount!==24*24||green!==64)throw Error('incomplete expected physical footprint');for(let repeat=0;repeat<2;repeat++){const next=await capture(true);if(next.some((v,i)=>v!==occluded[i]))throw Error('static card not bitwise');}results.push({samples,mrt,greenWitnesses:green,redWitnesses:redCount,staticBitwise:true});for(const t of[output,msaa,ambient,depth])t.destroy();
 }
 const validation=await device.popErrorScope();if(validation||errors.length)throw Error(JSON.stringify({validation:validation?.message,errors}));device.destroy();return{compiled,results};
}
const result=await withBrowser({path:'/recipe-board-proof',width:64,height:64},async({evaluate})=>evaluate('('+run.toString()+')('+JSON.stringify(sources)+')'));
assert.equal(result.compiled,12);assert.equal(result.results.length,4);await writeFile('.local/recipe-boards-dawn-proof.json',JSON.stringify(result,null,2)+'\n');console.log('DAWN_RECIPE_BOARDS_OK '+JSON.stringify(result));
