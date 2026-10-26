// Exact primary Height shader plus the exact default Terrain vertex shader.
// Reference: the uncut Height grid, with the Unknown page removed explicitly.
// This tests the ground JOIN and Unknown ownership; Proxy is covered separately.
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
import {calibrateRasterPrecision, snapRasterCoordinate} from './raster_precision.mjs';
import {heightReferenceSupport} from './height_reference.mjs';
const inputs = {
  diagnostic: process.argv.includes('--diagnostic'),
  collect: process.argv.includes('--collect'),
  one: await readFile('.local/height-oracle/near-1.wgsl', 'utf8'),
  four: await readFile('.local/height-oracle/near-4.wgsl', 'utf8'),
  terrain: await readFile('.local/height-oracle/terrain.wgsl', 'utf8'),
  quad: JSON.parse(await readFile('.local/height-oracle/quad.json', 'utf8')),
  cameras: JSON.parse(await readFile('.local/height-oracle/cameras.json', 'utf8')),
};
async function oracle(input) {
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) throw Error('WebGPU unavailable');
  const device = await adapter.requestDevice(), errors = [], results = [];
  device.addEventListener('uncapturederror', event => errors.push(event.error.message));
  device.pushErrorScope('validation');
  const precision = await calibrateRasterPrecision(device);
  const B = GPUBufferUsage, T = GPUTextureUsage, W = 128, H = 128, N = 25;
  const module = async code => {
    const result = device.createShaderModule({code});
    const bad = (await result.getCompilationInfo()).messages.filter(x => x.type === 'error');
    if (bad.length) throw Error(JSON.stringify(bad));
    return result;
  };
  function buffer(bytes, usage) {
    const result = device.createBuffer({size: Math.max(4, bytes.byteLength), usage: usage | B.COPY_DST});
    device.queue.writeBuffer(result, 0, bytes); return result;
  }
  async function readBuffer(source, size) {
    const read = device.createBuffer({size,usage:B.COPY_DST|B.MAP_READ});
    const encoder=device.createCommandEncoder();encoder.copyBufferToBuffer(source,0,read,0,size);device.queue.submit([encoder.finish()]);
    await read.mapAsync(GPUMapMode.READ);const result=new Float32Array(read.getMappedRange()).slice();read.unmap();read.destroy();return result;
  }
  async function sampleLocations() {
    const shader=await module(`
@group(0)@binding(0)var<storage,read_write>rows:array<vec4<f32>,4>;
struct V{@builtin(position)p:vec4<f32>,@location(0)@interpolate(linear,sample)uv:vec2<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->V{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return V(vec4(p[i],0.,1.),vec2(p[i].x*.5+.5,.5-p[i].y*.5));}
@fragment fn fragment(v:V,@builtin(sample_index)i:u32)->@location(0)vec4<f32>{rows[i]=vec4(v.uv,fract(v.p.xy));return vec4(1.);}`);
    const pipeline=await device.createRenderPipelineAsync({layout:'auto',vertex:{module:shader,entryPoint:'vertex'},fragment:{module:shader,entryPoint:'fragment',targets:[{format:'rgba8unorm'}]},multisample:{count:4}});
    const rows=buffer(new Uint8Array(64),B.STORAGE|B.COPY_SRC),bind=device.createBindGroup({layout:pipeline.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:rows}}]});
    const texture=device.createTexture({size:[1,1],format:'rgba8unorm',sampleCount:4,usage:T.RENDER_ATTACHMENT});
    const encoder=device.createCommandEncoder(),pass=encoder.beginRenderPass({colorAttachments:[{view:texture.createView(),clearValue:[0,0,0,0],loadOp:'clear',storeOp:'store'}]});pass.setPipeline(pipeline);pass.setBindGroup(0,bind);pass.draw(3);pass.end();device.queue.submit([encoder.finish()]);
    const values=await readBuffer(rows,64);texture.destroy();rows.destroy();return Array.from({length:4},(_,i)=>Array.from(values.slice(i*4,i*4+4)));
  }
  const locations=await sampleLocations();
  const debugRows=buffer(new Uint8Array(640),B.STORAGE|B.COPY_SRC);
  const camera = buffer(new Uint8Array(96), B.UNIFORM);
  const terrainCamera = buffer(new Uint8Array(96), B.UNIFORM);
  const headers = new ArrayBuffer(N * 64), hf = new Float32Array(headers), hu = new Uint32Array(headers);
  const data = new Uint32Array(N * 819), df = new Float32Array(data.buffer);
  let tile = 0;
  for (let z = -2; z <= 2; z++) for (let x = -2; x <= 2; x++, tile++) {
    const h = tile * 16, d = tile * 819;
    hf.set([x * 16, 0, z * 16, 1], h); hu.set([1, 0, 0, 0], h + 4);
    hf.set([x * 16, 8, z * 16, 0], h + 8); hf.set([(x + 1) * 16, 8, (z + 1) * 16, 0], h + 12);
    df.fill(8, d, d + 289); data.fill(0xffffffff, d + 289, d + 299);
    data.fill(3, d + 299, d + 555); data.fill(0xffffffff, d + 555, d + 811);
  }
  const visible = Uint32Array.from({length: N * 2}, (_, i) => i % N);
  const tileBuffer = buffer(new Uint8Array(headers), B.STORAGE), payload = buffer(data, B.STORAGE), list = buffer(visible, B.STORAGE);
  const nearImage = new ArrayBuffer(4160), nf = new Float32Array(nearImage), nu = new Uint32Array(nearImage);
  const nearMask = buffer(new Uint8Array(nearImage), B.STORAGE);
  const atlas = device.createTexture({size: [1, 1, 4], format: 'rgba8unorm', usage: T.TEXTURE_BINDING | T.COPY_DST});
  device.queue.writeTexture({texture: atlas}, new Uint8Array(16).fill(255), {bytesPerRow: 4, rowsPerImage: 1}, [1, 1, 4]);
  const atlasView = atlas.createView({dimension: '2d-array'}), sampler = device.createSampler();
  // ClientView::new selects CompactUnit before uploads. Reuse its existing
  // packed descriptor/tile path and its unchanged 1/256-pixel raster guard.
  const planeModule = await module(input.terrain);
  const referenceData = buffer(data, B.STORAGE);
  const referenceMask = buffer(new Uint8Array(4160), B.STORAGE);
  const nearObjects = buffer(new Uint8Array(N * 128), B.STORAGE);
  const nearQuads = buffer(new Uint32Array(N * 4), B.STORAGE);
  const rasterFaces=256;
  const nearVisible = buffer(Uint32Array.from({length:N*rasterFaces},(_,i)=>((Math.floor(i/256)<<8)|(i%256))), B.STORAGE);
  function setNear(volume, missing) {
    const rows = [];
    for (let z = volume[0][2]; z < volume[1][2]; z++) for (let x = volume[0][0]; x < volume[1][0]; x++) {
      if (missing && x === 1 && z === 0) continue;
      if (volume[0][1] <= 0 && volume[1][1] > 0) rows.push(x * 16, z * 16, (x + 1) * 16, (z + 1) * 16);
    }
    if (rows.length) {
      const objects=new ArrayBuffer(N*128), f=new Float32Array(objects), u=new Uint32Array(objects), quads=new Uint32Array(N*4);
      for(let slot=0;slot<rows.length/4;slot++) {
        const base=slot*32;f.set([1,0,0,0,0,1,0,0,0,0,1,0,rows[slot*4],0,rows[slot*4+1],1],base);
        f.set([0,0,0,0,16,16,16,0],base+16);u.set([slot,1,0,0,0,0,1,1],base+24);
        quads.set(input.quad,slot*4);quads[slot*4+3]=slot;
      }
      device.queue.writeBuffer(nearObjects,0,objects);device.queue.writeBuffer(nearQuads,0,quads);
    }
    nf.fill(0); nf.set(volume[0].map(x => x * 16)); nf.set(volume[1].map(x => x * 16), 4); nu[3] = 1;
    device.queue.writeBuffer(nearMask, 0, nearImage); return rows.length / 4*rasterFaces;
  }
  for (const samples of [1, 4]) {
    if (input.diagnostic && samples === 1) continue;
    const four = input.four;
    let heightCode = input.diagnostic ? four
      .replace('if covered_by_near(sample_world-normalize(input.normal)*0.0001){discard;}', '')
      .replace('let ownership=height_cell_class(input.cell_min,input.cell_max);if ownership==1u||(ownership==2u&&covered_by_near(sample_world-normalize(input.normal)*0.0001)){discard;}', '')
      .replace('return vec4(texel.rgb*input.tint*illumination(sample_world,normalize(input.normal)),1.);', 'return vec4((sample_world.x+16.)*texel.r,sample_world.z,input.world_h.w,f32(sample)+1.);')
      : samples === 1 ? input.one : four;
    if (input.diagnostic) {
      heightCode='@group(0)@binding(22)var<storage,read_write>debugRows:array<vec4<f32>,40>;\n'+heightCode.replace(
        'return vec4((sample_world.x+16.)*texel.r,sample_world.z,input.world_h.w,f32(sample)+1.);',
        'let value=vec4((sample_world.x+16.)*texel.r,sample_world.z,input.world_h.w,f32(sample));let pixel=vec2<u32>(input.position.xy);let points=array<vec2<u32>,5>(vec2(46u,66u),vec2(45u,69u),vec2(41u,82u),vec2(42u,79u),vec2(109u,83u));for(var i=0u;i<5u;i++){if all(pixel==points[i]){let at=(i*4u+sample)*2u;debugRows[at]=value;debugRows[at+1u]=vec4(fract(input.position.xy),input.world_h.x,input.world_h.z);}}return vec4(value.xyz,1.);');
    }
    const heightShader = await module(heightCode);
    const make = source => device.createRenderPipelineAsync({layout: 'auto', vertex: {module: source, entryPoint: 'vertex'}, fragment: {module: source, entryPoint: 'fragment', targets: [{format: 'rgba16float'}]}, primitive: {cullMode: 'back',topology:source===planeModule?'triangle-strip':'triangle-list'}, depthStencil: {format: 'depth32float', depthWriteEnabled: true, depthCompare: 'greater-equal'}, multisample: {count: samples}});
    const height = await make(heightShader), plane = await make(planeModule);
    const entries = [
      [0, {buffer: camera}], [1, {buffer: tileBuffer}], [2, {buffer: payload}], [3, {buffer: list}],
      [4, atlasView], [5, sampler], [21, {buffer: nearMask}],
    ].map(([binding, resource]) => ({binding, resource})).filter(entry => !input.diagnostic || entry.binding !== 21);
    if(input.diagnostic)entries.push({binding:22,resource:{buffer:debugRows}});
    const bind = device.createBindGroup({layout: height.getBindGroupLayout(0), entries});
    const referenceBind = device.createBindGroup({layout: height.getBindGroupLayout(0), entries: entries.map(entry => entry.binding === 2 ? {binding:2,resource:{buffer:referenceData}} : entry.binding === 21 ? {binding:21,resource:{buffer:referenceMask}} : entry)});
    const nearBind = device.createBindGroup({layout:plane.getBindGroupLayout(0),entries:[{binding:0,resource:{buffer:terrainCamera}},{binding:1,resource:{buffer:nearObjects}},{binding:2,resource:{buffer:nearQuads}},{binding:3,resource:{buffer:nearVisible}}]});
    const color = device.createTexture({size: [W, H], format: 'rgba16float', sampleCount: samples, usage: T.RENDER_ATTACHMENT | T.TEXTURE_BINDING});
    const depth = device.createTexture({size: [W, H], format: 'depth32float', sampleCount: samples, usage: T.RENDER_ATTACHMENT | T.TEXTURE_BINDING});
    const depthView = depth.createView(), colorView = color.createView();
    const channels = input.diagnostic ? 4 : 2;
    const readShader = await module(`
@group(0)@binding(0)var depth:${input.diagnostic ? 'texture_multisampled_2d<f32>' : samples === 4 ? 'texture_depth_multisampled_2d' : 'texture_depth_2d'};
@group(0)@binding(1)var<storage,read_write>values:array<f32>;
${input.diagnostic?'':`@group(0)@binding(2)var color:${samples===4?'texture_multisampled_2d<f32>':'texture_2d<f32>'};`}
@compute @workgroup_size(64)fn read(@builtin(global_invocation_id)q:vec3<u32>){let i=q.x;if i>=${W * H * samples}u{return;}let p=vec2<i32>(i32((i/${samples}u)%${W}u),i32(i/${W * samples}u));${input.diagnostic ? 'let value=textureLoad(depth,p,i32(i%4u));for(var c=0u;c<4u;c++){values[i*4u+c]=value[c];}' : `values[i*2u]=textureLoad(depth,p,${samples === 4 ? 'i32(i%4u)' : '0'});values[i*2u+1u]=textureLoad(color,p,${samples===4?'i32(i%4u)':'0'}).a;`}}`);
    const compute = await device.createComputePipelineAsync({layout: 'auto', compute: {module: readShader, entryPoint: 'read'}});
    const values = device.createBuffer({size: W * H * samples * 4 * channels, usage: B.STORAGE | B.COPY_SRC});
    const output = device.createBuffer({size: W * H * samples * 4 * channels, usage: B.COPY_DST | B.MAP_READ});
    const readEntries=[{binding:0,resource:input.diagnostic?colorView:depthView},{binding:1,resource:{buffer:values}}];if(!input.diagnostic)readEntries.push({binding:2,resource:colorView});
    const readBind = device.createBindGroup({layout: compute.getBindGroupLayout(0), entries:readEntries});
    async function frame(nearCount, full) {
      device.queue.writeBuffer(camera,68,new Uint32Array([nearCount]));
      const encoder = device.createCommandEncoder();
      const pass = encoder.beginRenderPass({colorAttachments: [{view: colorView, clearValue: [0, 0, 0, 0], loadOp: 'clear', storeOp: 'store'}], depthStencilAttachment: {view: depthView, depthClearValue: 0, depthLoadOp: 'clear', depthStoreOp: 'store'}});
      if (full) {pass.setPipeline(plane); pass.setBindGroup(0, nearBind); pass.draw(4,nearCount);}
      else {if (nearCount) {pass.setPipeline(plane); pass.setBindGroup(0, nearBind); pass.draw(4,nearCount);}pass.setPipeline(height); pass.setBindGroup(0, bind); pass.draw(1536, N);}
      pass.end(); const read = encoder.beginComputePass(); read.setPipeline(compute); read.setBindGroup(0, readBind); read.dispatchWorkgroups(Math.ceil(W * H * samples / 64)); read.end();
      encoder.copyBufferToBuffer(values, 0, output, 0, W * H * samples * 4 * channels); device.queue.submit([encoder.finish()]); await output.mapAsync(GPUMapMode.READ);
      const result = new Float32Array(output.getMappedRange()).slice(); output.unmap();
      if(input.diagnostic)return result;
      return {depth:Float32Array.from({length:result.length/2},(_,i)=>result[i*2]),alpha:Float32Array.from({length:result.length/2},(_,i)=>result[i*2+1])};
    }
    for (const [cameraIndex, matrix] of input.cameras.entries()) {
      if (input.diagnostic && cameraIndex !== 1) continue;
      const image = new ArrayBuffer(96), f = new Float32Array(image), u = new Uint32Array(image); f.set(matrix.flat()); u[16] = N; u[20] = samples; device.queue.writeBuffer(camera, 0, image);
      const terrainImage=new ArrayBuffer(96),tf=new Float32Array(terrainImage),tu=new Uint32Array(terrainImage);tf.set(matrix.flat());tf.set([W,H,0,0],16);tu[21]=N*rasterFaces;tu[22]=2;device.queue.writeBuffer(terrainCamera,0,terrainImage);
      const support=heightReferenceSupport(matrix,[W,H],samples===1?[[.5,.5]]:locations.map(row=>row.slice(0,2)),precision,[-32,-32,48,48]);
      if (input.diagnostic) {
        setNear([[-1,0,-1],[2,3,2]],false);
        await frame(0,false);
        const actual=await readBuffer(debugRows,640);
        const diagnostic = [[46,66],[45,69],[41,82],[42,79],[109,83]].map(([x,y],pixel) => ({pixel:[x,y],samples:Array.from({length:4},(_,sample)=>({world:Array.from(actual.slice((pixel*4+sample)*8,(pixel*4+sample)*8+4)),fragmentAndRaw:Array.from(actual.slice((pixel*4+sample)*8+4,(pixel*4+sample)*8+8))}))}));
        const validation = await device.popErrorScope();
        if (validation || errors.length) throw Error(JSON.stringify({validation:validation?.message,errors}));
        device.destroy();return {diagnostic,sampleLocations:locations,precision,source:'instrumented exact primary Height f32 storage world coordinates; no coverage acceptance claim'};
      }
      for (const [name, volume, missing] of [
        ['low', [[-1,0,-1],[2,3,2]], false],
        ['high', [[-1,5,-1],[2,8,2]], false],
        ['unknown', [[-1,0,-1],[2,3,2]], true],
      ]) {
        const referenceCount=setNear([[-2,0,-2],[3,3,3]],missing);
        const referenceImage = await frame(referenceCount, true),reference=referenceImage.depth;
        const count = setNear(volume, missing), actualImage = await frame(count, false),actual=actualImage.depth;
        let holes = 0, extra = 0, depthErrors = 0, maxError = 0, referenceHoles=0, supportSamples=0,alphaErrors=0;
        const witnesses = [];
        for (let i = 0; i < actual.length; i++) {
          const pixel = Math.floor(i/samples), x=pixel%W, y=Math.floor(pixel/W), lane=i%samples;
          // The independent mask excludes only the measured external-source
          // silhouette uncertainty. It has no Near/Unknown/shader inputs.
          if(support.mask[i]!==2)continue;
          supportSamples++;if(!missing&&reference[i]===0)referenceHoles++;
          alphaErrors+=Number(referenceImage.alpha[i]!==actualImage.alpha[i]);
          if(referenceImage.alpha[i]!==Number(reference[i]>0)||actualImage.alpha[i]!==Number(actual[i]>0))throw Error('Opaque sample depth/alpha consistency failed');
          holes += Number(reference[i] > 0 && actual[i] === 0); extra += Number(reference[i] === 0 && actual[i] > 0);
          const error = Math.abs(actual[i] - reference[i]); maxError = Math.max(maxError, error); depthErrors += Number(error > 1e-6);
          if (error > 1e-6 && witnesses.length < 8) {
            const pixel = Math.floor(i / samples), x = pixel % W, y = Math.floor(pixel / W), lane = i % samples;
            witnesses.push({x,y,lane,expected:reference[i],actual:actual[i]});
          }
        }
        const negativeWitness=samples===1&&cameraIndex===1&&name==='low'?{pixel:[43,75],support:support.mask[75*W+43],reference:reference[75*W+43],actual:actual[75*W+43]}:undefined;
        const failed=Boolean(holes||extra||depthErrors||referenceHoles||alphaErrors);
        const row={samples,cameraIndex,name,comparedSamples:supportSamples,maxError,holes,extra,depthErrors,referenceHoles,alphaErrors,failed,witnesses,support:support.report,negativeWitness};
        if(failed&&!input.collect)throw Error(JSON.stringify(row));
        results.push(row);
      }
    }
  }
  const validation = await device.popErrorScope();
  if (validation || errors.length) throw Error(JSON.stringify({validation:validation?.message,errors}));
  device.destroy(); return {results,failureCount:results.filter(row=>row.failed).length,precision,rasterization:'ClientView CompactUnit (shipping default)',rasterGuardPixels:1/256,source:'exact primary Height and default Terrain vertex shaders/QuadDescriptor; authored opaque FS; independent full Terrain/f64 finite support/Unknown reference; Proxy native full JOIN is separate'};
}
const result = await withBrowser({path:'/height-join-oracle',width:128,height:128}, async ({evaluate}) =>
  evaluate('(async()=>{'+snapRasterCoordinate.toString()+';'+calibrateRasterPrecision.toString()+';'+heightReferenceSupport.toString()+';return await ('+oracle.toString()+')('+JSON.stringify(inputs)+');})()'));
if (inputs.diagnostic) {
  assert.equal(result.diagnostic.length,5);
  await writeFile('.local/height-join-dawn-diagnostic.json',JSON.stringify(result,null,2)+'\n');
  console.log('DAWN_HEIGHT_DIAGNOSTIC '+JSON.stringify(result));
} else {
  assert.equal(result.results.length,18);
  assert.equal(result.rasterization,'ClientView CompactUnit (shipping default)');
  assert.equal(result.rasterGuardPixels,1/256);
  await writeFile('.local/height-join-dawn-proof.json',JSON.stringify(result,null,2)+'\n');
  assert.equal(result.failureCount,0);
  console.log('DAWN_HEIGHT_JOIN_OK '+JSON.stringify(result));
}
