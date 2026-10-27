// Exact production resolve; independent authored motion/identity/HDR image oracle.
// This gate does not certify the separate model/terrain motion-vector source.
import assert from 'node:assert/strict';
import {readFile, writeFile} from 'node:fs/promises';
import {withBrowser} from './browser.mjs';
const inputs = {
  production: await readFile('.local/taa-oracle/resolve-stats.wgsl', 'utf8'),
  uninstrumented: await readFile('.local/taa-oracle/resolve.wgsl', 'utf8'),
};
async function oracle(input) {
  const adapter = await navigator.gpu.requestAdapter();
  if (!adapter) throw Error('WebGPU unavailable');
  const device = await adapter.requestDevice(), errors = [];
  device.addEventListener('uncapturederror', event => errors.push(event.error.message));
  device.pushErrorScope('validation');
  const B = GPUBufferUsage, T = GPUTextureUsage, W = 32, H = 16, results = [];
  const module = async code => {
    const value = device.createShaderModule({code});
    const bad = (await value.getCompilationInfo()).messages.filter(message => message.type === 'error');
    if (bad.length) throw Error(JSON.stringify(bad));
    return value;
  };
  const buffer = (size, usage) => device.createBuffer({size, usage});
  const texture = format => device.createTexture({size: [W, H], format, usage: T.RENDER_ATTACHMENT | T.TEXTURE_BINDING | T.COPY_SRC | T.COPY_DST});
  const targets = formats => formats.map(format => ({format}));
  const pipeline = async (source, formats, depth = false) => device.createRenderPipelineAsync({
    layout: 'auto', vertex: {module: source, entryPoint: 'vertex'},
    fragment: {module: source, entryPoint: 'fragment', targets: targets(formats)},
    primitive: {}, ...(depth ? {depthStencil: {format: 'depth32float', depthWriteEnabled: true, depthCompare: 'always'}} : {}),
  });
  const production = await module(input.production);
  // Compile the actual no-statistics variant too: shipping does not serialize
  // every fragment on the diagnostic global atomic counters.
  await pipeline(await module(input.uninstrumented), ['rgba16float', 'r32float', 'rgba32uint']);
  const resolve = await pipeline(production, ['rgba16float', 'r32float', 'rgba32uint']);
  const fixture = await module(`
struct Source { color:vec4<f32>, motion:vec4<f32>, identity:vec4<u32>, other:vec4<f32> };
@group(0) @binding(0) var<uniform> s:Source;
@vertex fn vertex(@builtin(vertex_index)i:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));return vec4(p[i],0.,1.);}
struct Out { @location(0)color:vec4<f32>, @location(1)motion:vec4<f32>, @location(2)identity:vec4<u32>, @builtin(frag_depth)depth:f32 };
@fragment fn fragment(@builtin(position)p:vec4<f32>)->Out {let odd=((u32(p.x)+u32(s.other.y))&1u)!=0u;return Out(select(vec4(s.color.r,0.,0.,1.),vec4(0.,0.,s.color.b,1.),odd),s.motion,s.identity,s.other.x);}`);
  const sourcePipeline = await pipeline(fixture, ['rgba16float', 'rgba16float', 'rgba32uint'], true);
  const sourceUniform = buffer(64, B.UNIFORM | B.COPY_DST), parameters = buffer(64, B.UNIFORM | B.COPY_DST);
  const stats = buffer(16, B.STORAGE | B.COPY_SRC | B.COPY_DST);
  const sourceBind = device.createBindGroup({layout: sourcePipeline.getBindGroupLayout(0), entries: [{binding: 0, resource: {buffer: sourceUniform}}]});
  const source = ['rgba16float', 'rgba16float', 'rgba32uint', 'depth32float'].map(texture);
  const output = texture('rgba16float');
  const history = [0, 1].map(() => ['rgba16float', 'r32float', 'rgba32uint'].map(texture));
  const bind = history.map(front => device.createBindGroup({layout: resolve.getBindGroupLayout(0), entries: [
    {binding: 0, resource: {buffer: parameters}},
    ...[source[0], source[3], source[1], source[2], ...front].map((value, index) => ({binding: index + 1, resource: value.createView()})),
    {binding: 8, resource: {buffer: stats}},
  ]}));
  const half = word => {
    const sign = word & 0x8000 ? -1 : 1, exponent = word >> 10 & 31, mantissa = word & 1023;
    return sign * (exponent ? exponent === 31 ? mantissa ? NaN : Infinity : 2 ** (exponent - 15) * (1 + mantissa / 1024) : 2 ** -14 * mantissa / 1024);
  };
  const attachment = value => ({view: value.createView(), loadOp: 'clear', clearValue: [0, 0, 0, 0], storeOp: 'store'});
  async function frame({phase, mode, available, back}) {
    const data = new ArrayBuffer(64), f = new Float32Array(data), u = new Uint32Array(data);
    f.set([17, 0, 17, 1]); f.set([0, 0, mode === 'far' ? 0 : .5, 1], 4);
    u.set([0x01000001, 0xfedcba98, 0xff800001, 99], 8);
    f.set([mode === 'far' ? 0 : .5, phase, 0, 0], 12);
    if (phase) {
      if (mode === 'id') u[10] ^= 1;
      if (mode === 'part') u[8] ^= 1;
      if (mode === 'generation') u[11]++;
      if (mode === 'disocclusion') f[6] = .25;
      if (mode === 'translated') f[4] = -1 / W;
      if (mode === 'unsupported') f[7] = 0;
      if (mode === 'unknown') u[8] = 0;
      if (mode === 'too-far') f[4] = -2 / W;
    }
    device.queue.writeBuffer(sourceUniform, 0, data);
    const params = new ArrayBuffer(64), pf = new Float32Array(params), pu = new Uint32Array(params);
    pu.set([W, H, available ? 1 : 0, Number(available)]);
    pf.set([0, 0, available ? .5 : 0, mode === 'too-far' ? 1 : 128], 4);
    pf.set([1e-6, 1 / 1024, 0, 0], 8);
    device.queue.writeBuffer(parameters, 0, params);
    const encoder = device.createCommandEncoder(); encoder.clearBuffer(stats);
    let pass = encoder.beginRenderPass({colorAttachments: source.slice(0, 3).map(attachment), depthStencilAttachment: {view: source[3].createView(), depthClearValue: 0, depthLoadOp: 'clear', depthStoreOp: 'store'}});
    pass.setPipeline(sourcePipeline); pass.setBindGroup(0, sourceBind); pass.draw(3); pass.end();
    pass = encoder.beginRenderPass({colorAttachments: [output, history[back][1], history[back][2]].map(attachment)});
    pass.setPipeline(resolve); pass.setBindGroup(0, bind[1 - back]); pass.draw(3); pass.end();
    encoder.copyTextureToTexture({texture: output}, {texture: history[back][0]}, [W, H]);
    const read = buffer(W * H * 8 + 16, B.COPY_DST | B.MAP_READ);
    encoder.copyTextureToBuffer({texture: output}, {buffer: read, bytesPerRow: W * 8}, [W, H]);
    encoder.copyBufferToBuffer(stats, 0, read, W * H * 8, 16);
    device.queue.submit([encoder.finish()]); await read.mapAsync(GPUMapMode.READ);
    const mapped = read.getMappedRange(), image = Array.from(new Uint16Array(mapped, 0, W * H * 4), half), counts = Array.from(new Uint32Array(mapped, W * H * 8, 4));
    read.unmap(); read.destroy(); return {image, counts};
  }
  for (const mode of ['blend', 'id', 'part', 'generation', 'disocclusion', 'translated', 'far', 'camera-cut', 'edit', 'revoke', 'space', 'unsupported', 'unknown', 'too-far']) {
    await frame({phase: 0, mode, available: false, back: 0});
    const reset = ['camera-cut', 'edit', 'revoke', 'space'].includes(mode);
    const actual = await frame({phase: 1, mode, available: !reset, back: 1});
    let maxError = 0;
    // Independent closed-form checker translation: one-pixel motion aligns
    // identical old/new colors; zero motion blends the opposite phase to8.5.
    for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) {
      const purple = mode === 'blend' || mode === 'far';
      const expected = purple ? [8.5, 0, 8.5, 1] : (x & 1) ? [17, 0, 0, 1] : [0, 0, 17, 1];
      for (let channel = 0; channel < 4; channel++) maxError = Math.max(maxError, Math.abs(actual.image[(x + W * y) * 4 + channel] - expected[channel]));
    }
    if (maxError !== 0 || actual.counts[0] !== W * H || actual.counts[1] > W * H * 28) throw Error(JSON.stringify({mode, maxError, counts: actual.counts}));
    const expectedAccepted = mode === 'blend' || mode === 'far' ? W * H : mode === 'translated' ? (W - 1) * H : 0;
    if (actual.counts[2] !== expectedAccepted) throw Error(JSON.stringify({mode, expectedAccepted, counts: actual.counts}));
    results.push({mode, maxError, counts: actual.counts});
  }
  const validation = await device.popErrorScope();
  if (validation || errors.length) throw Error(JSON.stringify({validation: validation?.message, errors}));
  device.destroy(); return {results, pixels: W * H, pipelinesDuringFrames: 0, source: 'authored vectors; model motion is a separate required gate'};
}
const result = await withBrowser({path: '/temporal-resolve-oracle', width: 32, height: 16}, async ({evaluate}) => evaluate('(' + oracle.toString() + ')(' + JSON.stringify(inputs) + ')'));
assert.equal(result.results.length, 14);
await writeFile('.local/taa-dawn-proof.json', JSON.stringify(result, null, 2) + '\n');
console.log('DAWN_TAA_RESOLVE_OK ' + JSON.stringify(result));
