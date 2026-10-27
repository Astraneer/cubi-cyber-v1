// Run MODEL_MOTION_DUMP=.local/model-motion-oracle cargo test --locked -p
// render --test model_motion_gpu -- --test-threads=1 first. Inputs are actual
// constructor catalogs/shaders and previous PRESENTED64B records. Independent
// f64 interpolation uses observed actual VS clips and calibrated raster snapping;
// the native test separately checks ideal f64 articulated bone transforms.
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
import {calibrateRasterPrecision,snapRasterCoordinate} from './raster_precision.mjs';

const inputs = {};
inputs.diagnostic = process.env.MODEL_MOTION_DIAGNOSTIC === '1';
inputs.vertexDiagnostic = process.env.MODEL_MOTION_VERTEX_DIAGNOSTIC === '1';
inputs.vertexReference = true;
if (inputs.vertexDiagnostic) inputs.diagnostic = true;
for (const family of ['block', 'entity']) {
  const root = `.local/model-motion-oracle/${family}`, value = {};
  for (const name of ['color.wgsl', 'motion.wgsl', 'cull.wgsl']) value[name] = await readFile(`${root}/${name}`, 'utf8');
  for (const name of ['vertices.bin', 'ranges.bin', 'current.bin', 'previous.bin', 'matches.bin', 'parameters.bin', 'witnesses.bin', 'camera.bin', 'atlas.bin']) value[name] = (await readFile(`${root}/${name}`)).toString('base64');
  const header = await readFile(`${root}/atlas.bin`);
  for (let i = 0; i < header.readUInt32LE(8); i++) value[`atlas${i}.bin`] = (await readFile(`${root}/atlas${i}.bin`)).toString('base64');
  inputs[family] = value;
}

async function run(inputs) {
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) throw Error('WebGPU unavailable');
  const device = await adapter.requestDevice({requiredLimits: {maxStorageBuffersPerShaderStage: 8}});
  const errors = [], results = [], B = GPUBufferUsage, T = GPUTextureUsage, S = GPUShaderStage, W = 128, H = 128, capacity = 4;
  device.addEventListener('uncapturederror', event => errors.push(event.error.message));
  device.pushErrorScope('validation');
  const raster=await calibrateRasterPrecision(device);
  const decode = text => Uint8Array.from(atob(text), c => c.charCodeAt(0));
  const buffer = (data, usage) => {
    const size = typeof data === 'number' ? data : data.byteLength;
    const result = device.createBuffer({size, usage: usage | B.COPY_DST});
    if (typeof data !== 'number') device.queue.writeBuffer(result, 0, data);
    return result;
  };
  const module = async code => {
    const result = device.createShaderModule({code});
    const bad = (await result.getCompilationInfo()).messages.filter(m => m.type === 'error');
    if (bad.length) throw Error(JSON.stringify(bad));
    return result;
  };
  const be = (binding, visibility, type = 'read-only-storage') => ({binding, visibility, buffer: {type}});
  const entry = (binding, value) => ({binding, resource: {buffer: value}});
  const attachment = texture => ({view: texture.createView(), loadOp: 'clear', storeOp: 'store', clearValue: [0, 0, 0, 0]});
  const texture = format => device.createTexture({size: [W, H], format, usage: T.RENDER_ATTACHMENT | T.TEXTURE_BINDING});
  const attributes = [
    ['float32x3', 0], ['float32x3', 12], ['float32x2', 24], ['float32x3', 32],
    ['uint32', 44], ['uint32', 48], ['uint32', 52], ['float32', 56], ['uint32', 60],
  ].map(([format, offset], shaderLocation) => ({format, offset, shaderLocation}));
  const readModule = await module(`
struct Pixel{hdr:vec4<f32>,motion:vec4<f32>,surface:vec4<u32>,depth:vec4<f32>};
@group(0)@binding(0)var color:texture_2d<f32>;@group(0)@binding(1)var motion:texture_2d<f32>;@group(0)@binding(2)var surface:texture_2d<u32>;@group(0)@binding(3)var depth:texture_depth_2d;@group(0)@binding(4)var<storage,read_write>output:array<Pixel>;
@compute @workgroup_size(8,8)fn read(@builtin(global_invocation_id)p:vec3<u32>){if p.x<128u&&p.y<128u{let at=vec2<i32>(p.xy);output[p.x+128u*p.y]=Pixel(textureLoad(color,at,0),textureLoad(motion,at,0),textureLoad(surface,at,0),vec4(textureLoad(depth,at,0),0.,0.,0.));}}`);
  const readPipeline = await device.createComputePipelineAsync({layout: 'auto', compute: {module: readModule, entryPoint: 'read'}});

  for (const family of ['block', 'entity']) {
    const entity = family === 'entity', input = inputs[family], kinds = entity ? 8 : 17;
    const camera = buffer(decode(input['camera.bin']), B.UNIFORM), instances = buffer(decode(input['current.bin']), B.STORAGE);
    const vertices = buffer(decode(input['vertices.bin']), entity ? B.VERTEX : B.STORAGE);
    const ranges = buffer(decode(input['ranges.bin']), B.STORAGE), lights = buffer(new Uint32Array([15, 15, 15, 15]), B.STORAGE);
    const visible = buffer(kinds * capacity * 4, B.STORAGE), indirect = buffer(kinds * 16, B.STORAGE | B.INDIRECT);
    const previous = buffer(decode(input['previous.bin']), B.STORAGE), metadata = decode(input['matches.bin']);
    const matches = buffer(metadata, B.STORAGE), parameters = buffer(decode(input['parameters.bin']), B.UNIFORM), footprint = buffer(256, B.STORAGE);
    const header = new Uint32Array(decode(input['atlas.bin']).buffer), tile = header[0], layers = header[1], levels = header[2];
    const atlas = device.createTexture({size: [tile, tile, layers], mipLevelCount: levels, format: 'rgba8unorm-srgb', usage: T.TEXTURE_BINDING | T.COPY_DST});
    for (let level = 0; level < levels; level++) {
      const size = Math.max(1, tile >> level);
      device.queue.writeTexture({texture: atlas, mipLevel: level}, decode(input[`atlas${level}.bin`]), {bytesPerRow: size * 4, rowsPerImage: size}, [size, size, layers]);
    }
    const atlasView = atlas.createView({dimension: '2d-array'});
    const sampler = device.createSampler({magFilter: 'nearest', minFilter: 'nearest', mipmapFilter: 'nearest', addressModeU: entity ? 'clamp-to-edge' : 'repeat', addressModeV: entity ? 'clamp-to-edge' : 'repeat'});
    const v = S.VERTEX | (inputs.diagnostic && !entity ? S.COMPUTE : 0);
    const baseEntries = [be(0, v, 'uniform'), be(1, v), be(2, v), be(3, v), {binding: 4, visibility: S.FRAGMENT, texture: {sampleType: 'float', viewDimension: '2d-array'}}, {binding: 5, visibility: S.FRAGMENT, sampler: {type: 'filtering'}}];
    if (entity) baseEntries.push(...[6, 7].map(binding => ({binding, visibility: S.FRAGMENT, texture: {sampleType: 'float', viewDimension: '2d-array'}})));
    const baseLayout = device.createBindGroupLayout({entries: baseEntries});
    const temporalLayout = device.createBindGroupLayout({entries: [be(0, v | S.FRAGMENT, 'uniform'), be(1, v), be(2, v)]});
    const baseBindings = [entry(0, camera), entry(1, instances), entry(2, entity ? visible : vertices), entry(3, entity ? lights : visible), {binding: 4, resource: atlasView}, {binding: 5, resource: sampler}];
    if (entity) baseBindings.push({binding: 6, resource: atlasView}, {binding: 7, resource: atlasView});
    const base = device.createBindGroup({layout: baseLayout, entries: baseBindings});
    const temporal = device.createBindGroup({layout: temporalLayout, entries: [entry(0, parameters), entry(1, previous), entry(2, matches)]});
    const vertexCount = decode(input['vertices.bin']).byteLength / 64;
    const pointWidth=256,pointHeight=Math.ceil(vertexCount*2/pointWidth),pointPixels=pointWidth*pointHeight;
    const vertexRead = inputs.vertexReference ? buffer(pointPixels*32, B.MAP_READ) : null;
    const vertexTargets = inputs.vertexReference ? [0,1].map(()=>device.createTexture({size:[pointWidth,pointHeight],format:'rgba32float',usage:T.RENDER_ATTACHMENT|T.COPY_SRC})) : null;
    const colorModule = await module(input['color.wgsl']), motionModule = await module(input['motion.wgsl']), cullModule = await module(input['cull.wgsl']);
    const vertex = (module, entryPoint) => ({module, entryPoint, buffers: entity ? [{arrayStride: 64, attributes}] : []});
    const primitive = {cullMode: entity ? 'back' : 'none'};
    const color = await device.createRenderPipelineAsync({layout: device.createPipelineLayout({bindGroupLayouts: [baseLayout]}), vertex: vertex(colorModule, 'vertex'), primitive, depthStencil: {format: 'depth32float', depthWriteEnabled: true, depthCompare: 'greater-equal'}, fragment: {module: colorModule, entryPoint: 'fragment', targets: [{format: 'rgba16float'}]}});
    const motionLayout=device.createPipelineLayout({bindGroupLayouts:[baseLayout,temporalLayout]});
    const motion = await device.createRenderPipelineAsync({layout: motionLayout, vertex: vertex(motionModule, 'temporal_vertex'), primitive, depthStencil: {format: 'depth32float', depthWriteEnabled: false, depthCompare: 'equal'}, fragment: {module: motionModule, entryPoint: 'temporal_fragment', targets: [{format: 'rgba16float'}, {format: 'rgba32uint'}]}});
    let pointPipeline=null;
    if(inputs.vertexReference){
      const marker=' return output;\n}\n@fragment fn temporal_fragment';
      if(!input['motion.wgsl'].includes(marker))throw Error('actual model motion vertex marker changed');
      const source=input['motion.wgsl'].replace(marker,`let point=index*${vertexCount}u+vertex_index;let pixel=vec2(f32(point%256u)+.5,f32(point/256u)+.5);output.position=vec4(pixel/vec2(256.,${pointHeight}.)*vec2(2.,-2.)+vec2(-1.,1.),0.,1.);\n${marker}`)
        +'\nstruct DebugOutput{@location(0)current:vec4<f32>,@location(1)previous:vec4<f32>};@fragment fn debug_fragment(input:TemporalVertex)->DebugOutput{return DebugOutput(input.current_clip,input.previous_clip);}';
      const debug=await module(source);
      pointPipeline=await device.createRenderPipelineAsync({layout:motionLayout,vertex:vertex(debug,'temporal_vertex'),primitive:{topology:'point-list'},fragment:{module:debug,entryPoint:'debug_fragment',targets:[{format:'rgba32float'},{format:'rgba32float'}]}});
    }
    const cullLayout = device.createBindGroupLayout({entries: [be(0, S.COMPUTE, 'uniform'), be(1, S.COMPUTE), be(2, S.COMPUTE, 'storage'), be(3, S.COMPUTE, 'storage'), be(4, S.COMPUTE), be(5, S.COMPUTE, 'storage')]});
    const cullPipelineLayout = device.createPipelineLayout({bindGroupLayouts: [cullLayout]});
    const reset = await device.createComputePipelineAsync({layout: cullPipelineLayout, compute: {module: cullModule, entryPoint: 'reset'}});
    const cull = await device.createComputePipelineAsync({layout: cullPipelineLayout, compute: {module: cullModule, entryPoint: 'cull'}});
    const cullBind = device.createBindGroup({layout: cullLayout, entries: [entry(0, camera), entry(1, instances), entry(2, visible), entry(3, indirect), entry(4, ranges), entry(5, footprint)]});
    const hdr = texture('rgba16float'), depth = texture('depth32float'), vectors = texture('rgba16float'), surfaces = texture('rgba32uint');
    const output = buffer(W * H * 64, B.STORAGE | B.COPY_SRC), readback = buffer(W * H * 64, B.MAP_READ);
    const readBind = device.createBindGroup({layout: readPipeline.getBindGroupLayout(0), entries: [{binding: 0, resource: hdr.createView()}, {binding: 1, resource: vectors.createView()}, {binding: 2, resource: surfaces.createView()}, {binding: 3, resource: depth.createView()}, entry(4, output)]});
    let poseDiagnostic = null;
    if (inputs.diagnostic && !entity) {
      const count = decode(input['vertices.bin']).byteLength / 64;
      const data = buffer(count * 2 * 32, B.STORAGE | B.COPY_SRC), read = buffer(count * 2 * 32, B.MAP_READ);
      const diagnosticModule = await module(input['motion.wgsl'] + `
@group(2)@binding(0)var<storage,read_write>debug_poses:array<vec4<f32>>;
@compute @workgroup_size(64)fn debug_pose(@builtin(global_invocation_id)at:vec3<u32>){let count=arrayLength(&vertices);if at.x>=count*2u{return;}let row=at.x/count;let v=vertices[at.x%count];let matched=temporal_matches[row];debug_poses[at.x*2u]=vec4(temporal_block_pose(v,instances[row]).world,1.);debug_poses[at.x*2u+1u]=vec4(temporal_block_pose(v,temporal_previous_blocks[matched.previous_index]).world,1.);}`);
      const layout = device.createBindGroupLayout({entries: [be(0, S.COMPUTE, 'storage')]});
      const diagnostic = await device.createComputePipelineAsync({layout: device.createPipelineLayout({bindGroupLayouts: [baseLayout, temporalLayout, layout]}), compute: {module: diagnosticModule, entryPoint: 'debug_pose'}});
      const bind = device.createBindGroup({layout, entries: [entry(0, data)]});
      const encoder = device.createCommandEncoder(), pass = encoder.beginComputePass();pass.setPipeline(diagnostic);pass.setBindGroup(0, base);pass.setBindGroup(1, temporal);pass.setBindGroup(2, bind);pass.dispatchWorkgroups(Math.ceil(count * 2 / 64));pass.end();encoder.copyBufferToBuffer(data, 0, read, 0, count * 2 * 32);device.queue.submit([encoder.finish()]);await read.mapAsync(GPUMapMode.READ);poseDiagnostic = new Float32Array(read.getMappedRange().slice(0));read.unmap();
    }
    async function frame(valid, staticPose=false, jitter=[0,0], oldPose=false) {
      const records = metadata.slice(), words = new Uint32Array(records.buffer);
      if(oldPose){const source=new Uint32Array(metadata.buffer);for(let row=0;row<2;row++)words.set(source.subarray(row*8,row*8+8),source[row*8+4]*8);}
      if (!valid) for (let row = 0; row < 2; row++) {words[row * 8 + 4] = 0xffffffff; words[row * 8 + 5] = 0;}
      device.queue.writeBuffer(instances,0,decode(input[oldPose?'previous.bin':'current.bin']));
      device.queue.writeBuffer(previous,0,decode(input['previous.bin']));
      if (staticPose) {
        device.queue.writeBuffer(previous,0,decode(input['current.bin']));
        for(let row=0;row<2;row++)words[row*8+4]=row;
      }
      const cameraData=decode(input['camera.bin']),matrix=new Float32Array(cameraData.buffer);
      for(let column=0;column<4;column++){matrix[column*4]=Math.fround(matrix[column*4]+Math.fround(2*jitter[0]/W*matrix[column*4+3]));matrix[column*4+1]=Math.fround(matrix[column*4+1]-Math.fround(2*jitter[1]/H*matrix[column*4+3]));}
      device.queue.writeBuffer(camera,0,cameraData);
      const parameterData=decode(input['parameters.bin']);new Float32Array(parameterData.buffer).set(jitter,18);device.queue.writeBuffer(parameters,0,parameterData);
      device.queue.writeBuffer(matches, 0, records);
      const encoder = device.createCommandEncoder();
      let pass = encoder.beginComputePass();pass.setBindGroup(0, cullBind);pass.setPipeline(reset);pass.dispatchWorkgroups(1);pass.setPipeline(cull);pass.dispatchWorkgroups(1);pass.end();
      pass = encoder.beginRenderPass({colorAttachments: [attachment(hdr)], depthStencilAttachment: {view: depth.createView(), depthClearValue: 0, depthLoadOp: 'clear', depthStoreOp: 'store'}});
      pass.setPipeline(color);pass.setBindGroup(0, base);if (entity) pass.setVertexBuffer(0, vertices);
      for (let kind = 0; kind < kinds; kind++) pass.drawIndirect(indirect, kind * 16);pass.end();
      pass = encoder.beginRenderPass({colorAttachments: [attachment(vectors), attachment(surfaces)], depthStencilAttachment: {view: depth.createView(), depthReadOnly: true}});
      pass.setPipeline(motion);pass.setBindGroup(0, base);pass.setBindGroup(1, temporal);if (entity) pass.setVertexBuffer(0, vertices);
      for (let kind = 0; kind < kinds; kind++) pass.drawIndirect(indirect, kind * 16);pass.end();
      if(pointPipeline){pass=encoder.beginRenderPass({colorAttachments:vertexTargets.map(attachment)});pass.setPipeline(pointPipeline);pass.setBindGroup(0,base);pass.setBindGroup(1,temporal);if(entity)pass.setVertexBuffer(0,vertices);for(let kind=0;kind<kinds;kind++)pass.drawIndirect(indirect,kind*16);pass.end();for(let i=0;i<2;i++)encoder.copyTextureToBuffer({texture:vertexTargets[i]},{buffer:vertexRead,offset:i*pointPixels*16,bytesPerRow:pointWidth*16},[pointWidth,pointHeight]);}
      pass = encoder.beginComputePass();pass.setPipeline(readPipeline);pass.setBindGroup(0, readBind);pass.dispatchWorkgroups(W / 8, H / 8);pass.end();
      encoder.copyBufferToBuffer(output, 0, readback, 0, W * H * 64);device.queue.submit([encoder.finish()]);
      await readback.mapAsync(GPUMapMode.READ);const bytes = readback.getMappedRange().slice(0);readback.unmap();let poses=null;if(vertexRead){await vertexRead.mapAsync(GPUMapMode.READ);poses=new Float32Array(vertexRead.getMappedRange().slice(0));vertexRead.unmap();}return {f: new Float32Array(bytes), u: new Uint32Array(bytes),poses,jitter};
    }
    const bootstrap = await frame(false), actual = await frame(true);
    const stationary=await frame(true,true);let maxStaticPixels=0,staticNonzero=0;
    for(let pixel=0;pixel<W*H;pixel++)if(stationary.f[pixel*16+12]>0){const x=stationary.f[pixel*16+4],y=stationary.f[pixel*16+5];maxStaticPixels=Math.max(maxStaticPixels,Math.hypot(x,y)*W);if(x!==0||y!==0)staticNonzero++;}
    const staticControl={maxPixels:maxStaticPixels,nonzero:staticNonzero};
    if(staticNonzero&&!inputs.diagnostic)throw Error(JSON.stringify({family,staticControl}));
    const verify=(actual,checkBootstrap)=>{
    let depthPixels = 0, zeroCameraWrong = 0;
    for (let pixel = 0; pixel < W * H; pixel++) if (actual.f[pixel * 16 + 12] > 0) {
      depthPixels++;
      if (!actual.u[pixel * 16 + 11] || actual.f[pixel * 16 + 7] !== 1) throw Error(`${family} depth-equal hole${pixel}`);
      if (checkBootstrap&&(!bootstrap.u[pixel * 16 + 11] || bootstrap.f[pixel * 16 + 7] !== 0)) throw Error(`${family} bootstrap failed${pixel}`);
      if (Math.hypot(actual.f[pixel * 16 + 4], actual.f[pixel * 16 + 5]) > .002) zeroCameraWrong++;
    }
    const modelVertices=new DataView(decode(input['vertices.bin']).buffer),modelInstances=new DataView(decode(input['current.bin']).buffer),words=new Uint32Array(metadata.buffer);
    let count = 0, maxError = 0, badMotion = 0, badIdentity = 0;
    const failures = [];
    for(let pixel=0;pixel<W*H;pixel++){
      const at=pixel*16;if(actual.f[at+12]<=0)continue;
      const surface=Array.from(actual.u.slice(at+8,at+12)),triangle=surface[0]&0xffffff;
      const row=[0,1].find(row=>words[row*8+1]===surface[1]&&words[row*8+2]===surface[2]&&words[row*8+3]===surface[3]&&(words[row*8]|triangle)===surface[0]);
      if(row===undefined||triangle*3+2>=vertexCount)throw Error(`${family} unknown exact surface${pixel}`);
      if([0,1,2].some(v=>modelVertices.getUint32((triangle*3+v)*64+44,true)!==modelInstances.getUint32(row*64+(entity?32:16),true)))throw Error(`${family} surface names another model catalog kind`);
      const current=[0,1,2].map(v=>Array.from(actual.poses.slice((row*vertexCount+triangle*3+v)*4,(row*vertexCount+triangle*3+v)*4+4)));
      const previous=[0,1,2].map(v=>Array.from(actual.poses.slice(pointPixels*4+(row*vertexCount+triangle*3+v)*4,pointPixels*4+(row*vertexCount+triangle*3+v)*4+4)));
      if(current.some(v=>!(v[3]>0)))throw Error('Model reference fixture crossed the near clip plane');
      const screen=current.map(v=>[(v[0]/v[3]*.5+.5)*W,(.5-v[1]/v[3]*.5)*H].map(value=>snapRasterCoordinate(value,raster.subpixelBits,raster.rounding)));
      const [a,b,c]=screen,x=pixel%W+.5,y=Math.floor(pixel/W)+.5;
      const area=(b[1]-c[1])*(a[0]-c[0])+(c[0]-b[0])*(a[1]-c[1]);
      if(Math.abs(area)<1e-12)continue;
      const u=((b[1]-c[1])*(x-c[0])+(c[0]-b[0])*(y-c[1]))/area,v=((c[1]-a[1])*(x-c[0])+(a[0]-c[0])*(y-c[1]))/area;
      const weights=[u,v,1-u-v];if(weights.some(value=>value<.015))continue;
      const interpolate=points=>[0,1,2,3].map(channel=>weights.reduce((sum,weight,index)=>sum+weight*points[index][channel]/current[index][3],0));
      const now=interpolate(current),old=interpolate(previous);
      const wantMotion=[(old[0]/old[3]-now[0]/now[3])*.5+actual.jitter[0]/W,(now[1]/now[3]-old[1]/old[3])*.5+actual.jitter[1]/H,old[2]/old[3]];
      for (let channel = 0; channel < 3; channel++) {
        const want = wantMotion[channel], error = Math.abs(actual.f[at + 4 + channel] - want);
        maxError = Math.max(maxError, error);
        if (error > Math.abs(want) / 1024 + 2e-6) {
          badMotion++;
          if (failures.length < 3) failures.push({family, pixel, triangle, row, channel, actual: actual.f[at + 4 + channel], want, error, vertexClips:actual.poses?[0,1,2].map(v=>{const at=(row*vertexCount+triangle*3+v)*4;return[...actual.poses.slice(at,at+4),...actual.poses.slice(pointPixels*4+at,pointPixels*4+at+4)];}):null,poses: poseDiagnostic ? [0, 1, 2].map(v => Array.from(poseDiagnostic.slice(((row * decode(input['vertices.bin']).byteLength / 64 + triangle * 3 + v) * 8), ((row * decode(input['vertices.bin']).byteLength / 64 + triangle * 3 + v) * 8 + 8)))) : null});
          if (!inputs.diagnostic) throw Error(JSON.stringify(failures[0]));
        }
      }
      count++;
    }
    if (depthPixels < 300 || count < 600 || zeroCameraWrong < 150) throw Error(JSON.stringify({family, depthPixels, count, zeroCameraWrong}));
    return{family, raster,depthPixels, f64Witnesses: count, zeroCameraWrong, maxHalfError: maxError, badMotion, badIdentity, staticControl, failures, exactDepthCompare: 'equal', vertexStorageBuffers: 5, fragmentStorageBuffers: 0, pipelinesDuringFrames: 0};
    };
    results.push({control:'deformed',...verify(actual,true)});
    const oldJitter=[-.25,.125],newJitter=[.375,-.25];
    const oldPhysical=await frame(false,false,oldJitter,true),newPhysical=await frame(true,false,newJitter);
    let warp=0,withoutObjectMotion=0;
    const sameSurface=(pixel,uv)=>{if(uv.some(value=>value<0||value>=1))return false;const old=Math.floor(uv[0]*W)+Math.floor(uv[1]*H)*W;return[0,1,2,3].every(c=>oldPhysical.u[old*16+8+c]===newPhysical.u[pixel*16+8+c]);};
    for(let pixel=0;pixel<W*H;pixel++)if(newPhysical.f[pixel*16+12]>0){
      const base=[(pixel%W+.5-newJitter[0]+oldJitter[0])/W,(Math.floor(pixel/W)+.5-newJitter[1]+oldJitter[1])/H];
      if(sameSurface(pixel,[base[0]+newPhysical.f[pixel*16+4],base[1]+newPhysical.f[pixel*16+5]]))warp++;
      if(sameSurface(pixel,base))withoutObjectMotion++;
    }
    if(warp<300||warp<=withoutObjectMotion+50)throw Error(JSON.stringify({family,warp,withoutObjectMotion}));
    results.push({control:'physical-jitter-warp',oldJitter,newJitter,warp,withoutObjectMotion,...verify(newPhysical,false)});
  }
  const validation = await device.popErrorScope();
  if (validation || errors.length) throw Error(JSON.stringify({validation: validation?.message, errors}));
  device.destroy();return results;
}
const result = await withBrowser({path: '/actual-model-motion-oracle', width: 128, height: 128}, async ({evaluate}) => evaluate('(async()=>{'+snapRasterCoordinate.toString()+';'+calibrateRasterPrecision.toString()+';return(' + run.toString() + ')(' + JSON.stringify(inputs) + ');})()'));
assert.equal(result.length, 4);
if (inputs.diagnostic) {console.log('MODEL_MOTION_DIAGNOSTIC ' + JSON.stringify(result));process.exit(1);}
await writeFile('.local/model-motion-dawn-proof.json', JSON.stringify(result, null, 2) + '\n');
console.log('DAWN_ACTUAL_MODEL_MOTION_OK ' + JSON.stringify(result));
