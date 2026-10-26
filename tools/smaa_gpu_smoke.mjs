// Production WGSL + exact Rust-generated table, independent polygon reference.
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {withBrowser} from './browser.mjs';
const generated=spawnSync('cargo',['run','--locked','--quiet','-p','render','--example','smaa_area_table'],{env:{...process.env,CARGO_INCREMENTAL:'0'},maxBuffer:1<<20});
assert.equal(generated.status,0,generated.stderr.toString());
assert.equal(generated.stdout.byteLength,20808);
const table=Array.from(new Float32Array(generated.stdout.buffer,generated.stdout.byteOffset,generated.stdout.byteLength/4));
const sources=await Promise.all(['edges','weights','blend'].map(name=>readFile(`crates/render/src/smaa_${name}.wgsl`,'utf8')));
const result=await withBrowser({path:'/smaa-independent-oracle',width:128,height:128},async({evaluate})=>evaluate(`(async()=>{
 const adapter=await navigator.gpu.requestAdapter();if(!adapter)throw Error('WebGPU adapter unavailable');const device=await adapter.requestDevice();const errors=[];device.addEventListener('uncapturederror',e=>errors.push(e.error.message));
 const size=128,T=GPUTextureUsage,B=GPUBufferUsage;
 const texture=(format,usage)=>device.createTexture({size:[size,size],format,usage});
 const color=texture('rgba16float',T.COPY_DST|T.TEXTURE_BINDING),edges=texture('rg8unorm',T.RENDER_ATTACHMENT|T.TEXTURE_BINDING),weights=texture('rgba16float',T.RENDER_ATTACHMENT|T.TEXTURE_BINDING),output=texture('rgba8unorm',T.RENDER_ATTACHMENT|T.COPY_SRC);
 const params=device.createBuffer({size:16,usage:B.UNIFORM|B.COPY_DST}),parameterData=new ArrayBuffer(16);new Float32Array(parameterData).set([.1,2]);new Uint32Array(parameterData).set([16,8],2);device.queue.writeBuffer(params,0,parameterData);
 const area=device.createBuffer({size:20808,usage:B.STORAGE|B.COPY_DST});device.queue.writeBuffer(area,0,new Float32Array(${JSON.stringify(table)}));
 const shaderSources=${JSON.stringify(sources)};const pipelines=[];
 for(let i=0;i<3;i++){const module=device.createShaderModule({code:shaderSources[i]});const info=await module.getCompilationInfo();if(info.messages.some(m=>m.type==='error'))throw Error(JSON.stringify(info.messages.map(m=>({line:m.lineNum,message:m.message}))));pipelines.push(await device.createRenderPipelineAsync({layout:'auto',vertex:{module,entryPoint:'vertex'},fragment:{module,entryPoint:'fragment',targets:[{format:['rg8unorm','rgba16float','rgba8unorm'][i]}]}}));}
 const binds=[device.createBindGroup({layout:pipelines[0].getBindGroupLayout(0),entries:[{binding:0,resource:color.createView()},{binding:1,resource:{buffer:params}}]}),device.createBindGroup({layout:pipelines[1].getBindGroupLayout(0),entries:[{binding:0,resource:edges.createView()},{binding:1,resource:{buffer:area}},{binding:2,resource:{buffer:params}}]}),device.createBindGroup({layout:pipelines[2].getBindGroupLayout(0),entries:[{binding:0,resource:color.createView()},{binding:1,resource:weights.createView()}]})];
 const read=device.createBuffer({size:size*size*4,usage:B.COPY_DST|B.MAP_READ});
 async function capture(raw){const half=new Uint16Array(size*size*4);for(let i=0;i<raw.length;i++){half[i*4]=half[i*4+1]=half[i*4+2]=raw[i]?0x3c00:0;half[i*4+3]=0x3c00;}device.queue.writeTexture({texture:color},half,{bytesPerRow:size*8},[size,size]);const e=device.createCommandEncoder();for(let i=0;i<3;i++){const p=e.beginRenderPass({colorAttachments:[{view:[edges,weights,output][i].createView(),clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'}]});p.setPipeline(pipelines[i]);p.setBindGroup(0,binds[i]);p.draw(3);p.end();}e.copyTextureToBuffer({texture:output},{buffer:read,bytesPerRow:size*4},[size,size]);device.queue.submit([e.finish()]);await read.mapAsync(GPUMapMode.READ);const data=new Uint8Array(read.getMappedRange().slice(0));read.unmap();return data;}
 // Independent f64 polygon clipping; no positive-part integral or LUT indexing.
 function polygonArea(polygon){let sum=0;for(let i=0;i<polygon.length;i++){const p=polygon[i],q=polygon[(i+1)%polygon.length];sum+=p[0]*q[1]-q[0]*p[1];}return Math.abs(sum)*.5;}
 function clipped(low,high,left,right,above){const sign=p=>(right[0]-left[0])*(p[1]-left[1])-(right[1]-left[1])*(p[0]-left[0]),inside=v=>above?v<=0:v>=0;const polygon=[[-.5,low],[.5,low],[.5,high],[-.5,high]],out=[];let previous=polygon[3],pv=sign(previous);for(const p of polygon){const v=sign(p);if(inside(pv)!==inside(v)){const f=pv/(pv-v);out.push([previous[0]+f*(p[0]-previous[0]),previous[1]+f*(p[1]-previous[1])]);}if(inside(v))out.push(p);previous=p;pv=v;}return out.length<3?0:polygonArea(out);}
 const areaReference=(d0,d1)=>[clipped(0,1,[-d0-.5,-.5],[d1+.5,.5],true),clipped(-1,0,[-d0-.5,-.5],[d1+.5,.5],false)];
 const coverage=(slope,offset,x,y)=>clipped(-.5,.5,[-.5,slope*(x-.5)+offset-y],[.5,slope*(x+.5)+offset-y],false);
 const transform=(x,y,r)=>r===0?[x,y]:r===1?[y,size-1-x]:r===2?[size-1-x,size-1-y]:[size-1-y,x];
 let cases=0,maxError=0,witnesses=0;
 for(const run of [1,4,16])for(let rotation=0;rotation<4;rotation++)for(const invert of [false,true]){const offset=run===1?0:32,raw=Array.from({length:size*size},(_,i)=>{const[x,y]=transform(i%size,Math.floor(i/size),rotation);return (y>=Math.floor(x/run)+offset)!==invert;}),actual=await capture(raw);for(let y=20;y<size-20;y++)for(let x=20;x<size-20;x++){const[bx,by]=transform(x,y,rotation),boundary=Math.floor(bx/run)+offset,r=bx%run,a=areaReference(r,run-1-r);let expected=by>=boundary?1:0;if(by===boundary)expected=1-a[0];else if(by+1===boundary)expected=a[1];if(invert)expected=1-expected;const error=Math.abs(actual[(y*size+x)*4]-Math.round(expected*255));maxError=Math.max(maxError,error);if(error>1)throw Error(JSON.stringify({run,rotation,invert,x,y,error}));witnesses++;}cases++;}
 for(let kind=0;kind<6;kind++){const raw=Array.from({length:size*size},(_,i)=>{const x=i%size,y=Math.floor(i/size);return kind===0?false:kind===1?x>=64:kind===2?y>=64:kind===3?x>=20&&x<100&&y>=20&&y<40:kind===4?x===64:(x+y)%2===0;}),actual=await capture(raw);for(let i=0;i<raw.length;i++)if(actual[i*4]!==+raw[i]*255)throw Error('axis/corner/thin/checker detail blurred');cases++;}
 let previous=null,oldRaw=null,aaError=0,rawError=0,aaJump=0,rawJump=0;
 for(let phase=0;phase<=16;phase++){const offset=-.5+phase/16,raw=Array.from({length:size*size},(_,i)=>Math.floor(i/size)>=i%size+offset),actual=await capture(raw);for(let y=20;y<size-20;y++)for(let x=20;x<size-20;x++){const i=y*size+x,value=actual[i*4]/255,truth=coverage(1,offset,x,y);aaError+=(value-truth)**2;rawError+=(+raw[i]-truth)**2;if(previous){aaJump=Math.max(aaJump,Math.abs(value-previous[i*4]/255));rawJump=Math.max(rawJump,Math.abs(+raw[i]-oldRaw[i]));}}previous=actual;oldRaw=raw;}
 if(!(aaError<rawError*.9&&aaJump<.76&&rawJump===1))throw Error(JSON.stringify({aaError,rawError,aaJump,rawJump}));
 const last=await capture(oldRaw);if(last.some((x,i)=>x!==previous[i]))throw Error('static repeat changed');if(errors.length)throw Error(JSON.stringify(errors));device.destroy();return {cases,witnesses,maxByteError:maxError,aaSquaredError:aaError,rawSquaredError:rawError,maximumAaJump:aaJump,maximumRawJump:rawJump,staticBitwise:true,shaderErrors:0,pipelines:3};
})()`));
assert.equal(result.pipelines,3);console.log('DAWN_SMAA_POLYGON_TEMPORAL_OK '+JSON.stringify(result));
