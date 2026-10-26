// Independent startup-only raster interpolation calibration. No production
// coordinates, driver name or assumed subpixel grid enters the reference.
export function snapRasterCoordinate(value, bits, rounding) {
  const scale = 2 ** bits, scaled = value * scale;
  if (rounding === 'floor') return Math.floor(scaled) / scale;
  if (rounding === 'ceil') return Math.ceil(scaled) / scale;
  if (rounding === 'nearest-even') {
    const lower = Math.floor(scaled), fraction = scaled - lower;
    return (fraction < .5 ? lower : fraction > .5 ? lower + 1 : lower % 2 === 0 ? lower : lower + 1) / scale;
  }
  if (rounding === 'nearest-up') return Math.floor(scaled + .5) / scale;
  throw Error('Unknown calibrated raster rounding');
}

export async function calibrateRasterPrecision(device) {
  const B = GPUBufferUsage, T = GPUTextureUsage, W = 32, H = 32;
  // Represented f32 clip inputs, including exact ties with both even/odd lower
  // lattice coordinates. All triangles stay inside clip space.
  const cases = [
    [[2.103515625, 3.244140625], [28.712890625, 4.607421875], [7.357421875, 28.271484375]],
    [[2.03125, 3.09375], [28.15625, 4.21875], [7.28125, 28.34375]],
    [[3.28125, 2.15625], [29.09375, 7.03125], [4.21875, 27.15625]],
  ];
  const module = device.createShaderModule({code: `
@group(0)@binding(0)var<uniform>points:array<vec4<f32>,3>;
struct Out{@builtin(position)position:vec4<f32>,@location(0)barycentric:vec3<f32>};
@vertex fn vertex(@builtin(vertex_index)i:u32)->Out{let basis=array<vec3<f32>,3>(vec3(1.,0.,0.),vec3(0.,1.,0.),vec3(0.,0.,1.));return Out(points[i],basis[i]);}
@fragment fn fragment(input:Out)->@location(0)vec4<f32>{return vec4(input.barycentric,1.);}`});
  const errors = (await module.getCompilationInfo()).messages.filter(message => message.type === 'error');
  if (errors.length) throw Error(JSON.stringify(errors));
  const pipeline = await device.createRenderPipelineAsync({layout: 'auto', vertex: {module, entryPoint: 'vertex'}, fragment: {module, entryPoint: 'fragment', targets: [{format: 'rgba32float'}]}});
  const uniform = device.createBuffer({size: 48, usage: B.UNIFORM | B.COPY_DST});
  const bind = device.createBindGroup({layout: pipeline.getBindGroupLayout(0), entries: [{binding: 0, resource: {buffer: uniform}}]});
  const texture = device.createTexture({size: [W, H], format: 'rgba32float', usage: T.RENDER_ATTACHMENT | T.COPY_SRC});
  const read = device.createBuffer({size: W * H * 16, usage: B.COPY_DST | B.MAP_READ});
  const weights = (vertices, x, y) => {
    const [a, b, c] = vertices, area = (b[1] - c[1]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[1] - c[1]);
    const u = ((b[1] - c[1]) * (x - c[0]) + (c[0] - b[0]) * (y - c[1])) / area;
    const v = ((c[1] - a[1]) * (x - c[0]) + (a[0] - c[0]) * (y - c[1])) / area;
    return [u, v, 1 - u - v];
  };
  const observations = [];
  for (const vertices of cases) {
    const data = new Float32Array(12);
    vertices.forEach((point, index) => data.set([point[0] / W * 2 - 1, 1 - point[1] / H * 2, 0, 1], index * 4));
    // Use the exact represented GPU inputs in the CPU projection too.
    const represented = [0, 1, 2].map(index => [(data[index * 4] * .5 + .5) * W, (.5 - data[index * 4 + 1] * .5) * H]);
    device.queue.writeBuffer(uniform, 0, data);
    const encoder = device.createCommandEncoder(), pass = encoder.beginRenderPass({colorAttachments: [{view: texture.createView(), clearValue: [0, 0, 0, 0], loadOp: 'clear', storeOp: 'store'}]});
    pass.setPipeline(pipeline);pass.setBindGroup(0, bind);pass.draw(3);pass.end();
    encoder.copyTextureToBuffer({texture}, {buffer: read, bytesPerRow: W * 16}, [W, H]);device.queue.submit([encoder.finish()]);
    await read.mapAsync(GPUMapMode.READ);const image = new Float32Array(read.getMappedRange().slice(0));read.unmap();
    const samples = [];
    for (let y = 6; y < 24; y += 2) for (let x = 6; x < 24; x += 2) {
      if (weights(represented, x + .5, y + .5).some(value => value < .15)) continue;
      const actual = Array.from(image.slice((x + y * W) * 4, (x + y * W) * 4 + 3));
      if (actual.some(value => !(value > 0))) throw Error('Raster calibration interior coverage failed');
      samples.push({pixel: [x + .5, y + .5], actual});
    }
    if (samples.length < 15) throw Error('Insufficient independent raster calibration witnesses');
    observations.push({vertices: represented, samples});
  }
  const matches = [];
  for (let bits = 4; bits <= 16; bits++) for (const rounding of ['nearest-even', 'nearest-up', 'floor', 'ceil']) {
    let maxError = 0;
    for (const {vertices, samples} of observations) {
      const snapped = vertices.map(point => point.map(value => snapRasterCoordinate(value, bits, rounding)));
      for (const {pixel, actual} of samples) {
        const expected = weights(snapped, ...pixel);
        for (let channel = 0; channel < 3; channel++) maxError = Math.max(maxError, Math.abs(actual[channel] - expected[channel]));
      }
    }
    // This is an f32 interpolation measurement, not a model-motion tolerance.
    if (maxError <= 2e-6) matches.push({subpixelBits: bits, rounding, maxError});
  }
  uniform.destroy();read.destroy();texture.destroy();
  if (matches.length !== 1) throw Error('Raster precision was not uniquely calibrated: ' + JSON.stringify(matches));
  return {...matches[0], witnesses: observations.reduce((sum, observation) => sum + observation.samples.length, 0), triangles: observations.length};
}
