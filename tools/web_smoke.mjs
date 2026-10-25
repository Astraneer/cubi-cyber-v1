#!/usr/bin/env node
// Dependency-free Chrome DevTools smoke: a WebGPU adapter and actual pixels are
// mandatory. A served HTML document or an open browser process is not a pass.
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const headed = process.argv.slice(2).includes('--headed');
if (process.argv.slice(2).some(arg => arg !== '--headed')) throw new Error('Usage: node tools/web_smoke.mjs [--headed]');
const tmp = join(root, '.local', 'tmp');
await mkdir(tmp, { recursive: true });
const profile = await mkdtemp(join(tmp, 'webgpu-smoke-'));
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const timeoutMs = 90_000;
const deadline = Date.now() + timeoutMs;
let browser;
let server;
let socket;
let browserLog = '';
let serverLog = '';

function pngPixels(png) {
  if (!png.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]))) {
    throw new Error('Chromium screenshot is not a PNG');
  }
  let width;
  let height;
  let channels;
  const blocks = [];
  for (let offset = 8; offset < png.length;) {
    const size = png.readUInt32BE(offset);
    const type = png.toString('ascii', offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + size);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      channels = data[9] === 2 ? 3 : data[9] === 6 ? 4 : 0;
      if (data[8] !== 8 || data[12] !== 0 || !channels) throw new Error('Unsupported screenshot PNG format');
    }
    if (type === 'IDAT') blocks.push(data);
    offset += size + 12;
  }
  if (!width || !height || width > 4096 || height > 4096) throw new Error('Invalid screenshot dimensions');
  const stride = width * channels;
  const raw = inflateSync(Buffer.concat(blocks));
  if (raw.length !== (stride + 1) * height) throw new Error('Incomplete screenshot PNG data');
  const pixels = Buffer.alloc(stride * height);
  const paeth = (a, b, c) => {
    const p = a + b - c;
    const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
    return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
  };
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    if (filter > 4) throw new Error('Unsupported screenshot PNG filter');
    for (let x = 0; x < stride; x++) {
      const index = y * stride + x;
      const left = x >= channels ? pixels[index - channels] : 0;
      const up = y ? pixels[index - stride] : 0;
      const upperLeft = y && x >= channels ? pixels[index - stride - channels] : 0;
      const prediction = [0, left, up, Math.floor((left + up) / 2), paeth(left, up, upperLeft)][filter];
      pixels[index] = (raw[y * (stride + 1) + 1 + x] + prediction) & 255;
    }
  }
  return {
    width, height,
    sample(x, y) {
      const offset = (y * width + x) * channels;
      return [...pixels.subarray(offset, offset + 3), channels === 4 ? pixels[offset + 3] : 255];
    },
  };
}

async function stop(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  const stopped = once(child, 'exit').catch(() => {});
  child.kill('SIGTERM');
  await Promise.race([stopped, sleep(3000)]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await stopped;
  }
}

try {
  server = spawn(process.env.PYTHON ?? 'python3', ['tools/serve_web.py', '--port', '0'], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, TMPDIR: tmp, PYTHONDONTWRITEBYTECODE: '1' },
  });
  server.on('error', err => { serverLog += String(err); });
  server.stdout.on('data', data => { serverLog += data; });
  server.stderr.on('data', data => { serverLog += data; });
  let port;
  while (Date.now() < deadline) {
    port = serverLog.match(/WEB_SERVER_PORT=(\d+)/)?.[1];
    if (port) break;
    if (server.exitCode !== null || !server.pid) throw new Error(`Web server failed: ${serverLog}`);
    await sleep(50);
  }
  if (!port) throw new Error(`Web server did not start: ${serverLog}`);
  const url = `http://127.0.0.1:${port}/?scene=sky`;
  const chrome = process.env.CHROMIUM ?? 'chromium';
  browser = spawn(chrome, [
    ...(headed ? [] : ['--headless=new']), '--no-sandbox', '--disable-dev-shm-usage',
    '--no-first-run', '--no-default-browser-check', '--disable-background-networking',
    '--run-all-compositor-stages-before-draw',
    '--enable-unsafe-webgpu', '--enable-unsafe-swiftshader',
    '--ignore-gpu-blocklist', '--enable-gpu-rasterization',
    '--use-angle=swiftshader', '--use-vulkan=swiftshader',
    '--enable-features=Vulkan', '--disable-vulkan-surface',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    `--crash-dumps-dir=${join(profile, 'crashes')}`, 'about:blank',
  ], {
    cwd: root, stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env, TMPDIR: tmp,
      XDG_CONFIG_HOME: join(root, '.local', 'config'),
      XDG_CACHE_HOME: join(root, '.local', 'cache'),
    },
  });
  browser.on('error', err => { browserLog += String(err); });
  browser.stderr.on('data', data => { browserLog = (browserLog + data).slice(-20_000); });
  browser.stdout.on('data', data => { browserLog = (browserLog + data).slice(-20_000); });
  let debugPort;
  while (Date.now() < deadline) {
    try {
      debugPort = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0];
      if (debugPort) break;
    } catch { /* Chrome creates this after the DevTools server is listening. */ }
    if (browser.exitCode !== null || !browser.pid) throw new Error(`Chromium failed: ${browserLog}`);
    await sleep(100);
  }
  if (!debugPort) throw new Error(`Chromium DevTools did not start: ${browserLog}`);
  const targets = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
  const page = targets.find(target => target.type === 'page');
  if (!page) throw new Error('Chromium has no debuggable page');
  socket = new WebSocket(page.webSocketDebuggerUrl);
  await Promise.race([
    once(socket, 'open'),
    sleep(10_000).then(() => { throw new Error('Chromium DevTools WebSocket timeout'); }),
  ]);
  let nextId = 0;
  const pending = new Map();
  const errors = [];
  socket.addEventListener('message', event => {
    const message = JSON.parse(event.data);
    if (message.id) {
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      if (message.error) request.reject(new Error(JSON.stringify(message.error)));
      else request.resolve(message.result);
    } else if (message.method === 'Runtime.exceptionThrown') {
      errors.push(JSON.stringify(message.params.exceptionDetails));
    }
  });
  function call(method, params = {}) {
    const id = ++nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 10_000);
      pending.set(id, {
        resolve: result => { clearTimeout(timer); resolve(result); },
        reject: error => { clearTimeout(timer); reject(error); },
      });
      socket.send(JSON.stringify({ id, method, params }));
    });
  }
  await call('Runtime.enable');
  await call('Page.enable');
  await call('Page.navigate', { url });
  let result;
  while (Date.now() < deadline) {
    const state = await call('Runtime.evaluate', {
      expression: `JSON.stringify({state:document.documentElement?.dataset.m0, result:window.__M0, error:document.querySelector('[role=alert]')?.textContent})`,
      returnByValue: true,
    });
    if (state.exceptionDetails) throw new Error(JSON.stringify(state.exceptionDetails));
    const value = state.result.value ? JSON.parse(state.result.value) : {};
    if (value.state === 'error' || value.result?.ok === false) throw new Error(JSON.stringify(value));
    if (value.state === 'ready' && value.result) { result = value.result; break; }
    if (errors.length) throw new Error(`Browser exception: ${errors.join('\n')}`);
    await sleep(100);
  }
  if (!result) throw new Error(`WebGPU scene did not become ready. ${errors.join('\n')}`);
  const top = result.top ?? result.pixels?.slice(0, 4);
  const bottom = result.bottom ?? result.pixels?.slice(4, 8);
  if (result.frames < 1 || result.sampleCount !== 4 || !Array.isArray(top) || !Array.isArray(bottom)) {
    throw new Error(`Incomplete GPU result: ${JSON.stringify(result)}`);
  }
  if (result.startupPipelineCompilations !== 1 || result.probePipelineCompilations !== 1
    || result.totalStartupPipelineCompilations !== 2 || result.framePipelineCompilations !== 0) {
    throw new Error(`Unexpected pipeline work during startup/frame: ${JSON.stringify(result)}`);
  }
  if (top.length !== 4 || bottom.length !== 4 || ![...top, ...bottom].every(v => Number.isInteger(v) && v >= 0 && v <= 255)) {
    throw new Error(`Invalid RGBA readback: ${JSON.stringify(result)}`);
  }
  if (top[3] !== 255 || bottom[3] !== 255 || Math.abs(top[0] - bottom[0]) + Math.abs(top[1] - bottom[1]) + Math.abs(top[2] - bottom[2]) < 30) {
    throw new Error(`Sky gradient missing in GPU readback: ${JSON.stringify(result)}`);
  }
  await call('Runtime.evaluate', {
    expression: `new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(resolve))))`,
    awaitPromise: true,
  });
  const canvasBox = await call('Runtime.evaluate', {
    expression: `(() => { const c=document.querySelector('canvas'); if (!c) return null; const r=c.getBoundingClientRect(); return {x:r.x,y:r.y,width:r.width,height:r.height,scale:1}; })()`,
    returnByValue: true,
  });
  const clip = canvasBox.result.value;
  if (!clip || clip.width < 16 || clip.height < 16) throw new Error('Presented canvas has no visible area');
  // Export the actual displayed canvas, not the independent offscreen probe.
  // This is the required headless frame path from SPEC §8.4. Chromium 154 can
  // export correct WebGPU pixels while its headless compositor screenshot omits
  // the accelerated layer; keep that separate diagnostic visible below.
  const exported = await call('Runtime.evaluate', {
    expression: `document.querySelector('canvas').toDataURL('image/png')`,
    returnByValue: true,
  });
  if (!exported.result.value?.startsWith('data:image/png;base64,')) throw new Error('Actual canvas did not export a PNG');
  const actualFrame = Buffer.from(exported.result.value.split(',')[1], 'base64');
  const actual = pngPixels(actualFrame);
  const srgbAttachment = result.outputFormat?.endsWith('Srgb') && result.outputTransfer === 'attachment';
  const srgbShader = ['Rgba8Unorm', 'Bgra8Unorm'].includes(result.outputFormat) && result.outputTransfer === 'shader-srgb';
  if (result.colorEncoding !== 'srgb' || !(srgbAttachment || srgbShader)) {
    throw new Error(`Canvas output must match the native sRGB presentation: ${JSON.stringify({ colorEncoding: result.colorEncoding, outputFormat: result.outputFormat, outputTransfer: result.outputTransfer })}`);
  }
  const encode = value => result.colorEncoding === 'srgb'
    ? (value <= 0.0031308 ? 12.92 * value : 1.055 * value ** (1 / 2.4) - 0.055)
    : value;
  const zenith = [0.08, 0.25, 0.60], horizon = [0.65, 0.82, 1.0];
  let checkedCanvasPixels = 0;
  for (const fractionY of [0.125, 0.25, 0.5, 0.75, 0.875]) {
    const y = Math.floor(actual.height * fractionY);
    const v = (y + 0.5) / actual.height;
    const blend = v * v * (3 - 2 * v);
    const expected = zenith.map((channel, i) => Math.round(255 * encode(channel + (horizon[i] - channel) * blend)));
    for (const fractionX of [0.25, 0.5, 0.75]) {
      const pixel = actual.sample(Math.floor(actual.width * fractionX), y);
      if (pixel[3] !== 255 || expected.some((value, i) => Math.abs(pixel[i] - value) > 3)) {
        throw new Error(`Actual canvas differs from sky shader: ${JSON.stringify({ y, pixel, expected })}`);
      }
      checkedCanvasPixels++;
    }
  }
  const artifacts = join(root, '.local', 'artifacts');
  await mkdir(artifacts, { recursive: true });
  await writeFile(join(artifacts, 'm0-web-canvas.png'), actualFrame);
  const capture = await call('Page.captureScreenshot', { format: 'png', clip, fromSurface: true });
  const screenshot = Buffer.from(capture.data, 'base64');
  await writeFile(join(artifacts, `m0-web-compositor-${headed ? 'headed' : 'headless'}.png`), screenshot);
  const image = pngPixels(screenshot);
  const canvasTop = image.sample(Math.floor(image.width / 2), Math.floor(image.height / 4));
  const canvasBottom = image.sample(Math.floor(image.width / 2), Math.floor(image.height * 3 / 4));
  const gradient = canvasTop.slice(0, 3).reduce((sum, channel, i) => sum + Math.abs(channel - canvasBottom[i]), 0);
  const compositorCaptured = canvasTop[3] === 255 && canvasBottom[3] === 255 && gradient >= 30 && canvasTop[2] > canvasTop[0];
  if (headed && !compositorCaptured) {
    throw new Error(`Presented canvas is not the sky gradient: ${JSON.stringify({ canvasTop, canvasBottom })}`);
  }
  if (!compositorCaptured) {
    console.warn(`HEADLESS_COMPOSITOR_CAPTURE_LIMITATION ${JSON.stringify({ canvasTop, canvasBottom })}; actual canvas PNG passed ${checkedCanvasPixels} shader reference samples.`);
  }
  // An unavailable WebGPU implementation must produce a human-readable error,
  // rather than an empty canvas or a silent WebGL fallback.
  await call('Page.addScriptToEvaluateOnNewDocument', {
    source: `Object.defineProperty(navigator, 'gpu', {value: undefined, configurable: true});`,
  });
  await call('Page.reload', { ignoreCache: true });
  let unsupported;
  const unsupportedDeadline = Date.now() + 10_000;
  while (Date.now() < unsupportedDeadline) {
    const state = await call('Runtime.evaluate', {
      expression: `JSON.stringify({gpuAbsent:navigator.gpu===undefined,state:document.documentElement?.dataset.m0,result:window.__M0,text:document.getElementById('status')?.textContent})`,
      returnByValue: true,
    });
    const value = state.result.value ? JSON.parse(state.result.value) : {};
    if (value.state === 'error' && value.gpuAbsent && value.result?.ok === false && value.text?.includes('WebGPU')) {
      unsupported = value.text;
      break;
    }
    await sleep(100);
  }
  if (!unsupported) throw new Error('Missing human-readable error when WebGPU is unavailable');
  console.log(`WEBGPU_SKY_OK ${JSON.stringify({ mode: headed ? 'headed' : 'headless', frames: result.frames, sampleCount: result.sampleCount, outputFormat: result.outputFormat, colorEncoding: result.colorEncoding, outputTransfer: result.outputTransfer, top, bottom, checkedCanvasPixels, compositorCaptured, canvasTop, canvasBottom, unsupported })}`);
} catch (error) {
  console.error(String(error));
  if (browserLog) console.error(browserLog);
  process.exitCode = 1;
} finally {
  if (socket) socket.close();
  await stop(browser);
  await stop(server);
  await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
