#!/usr/bin/env node
// Real Web Workers, actual WASM public generation, and a native byte reference.
// Run after tools/build_web.sh, with tools/env.sh sourced.
import { spawn, execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdir, mkdtemp, rm, realpath } from 'node:fs/promises';
import { dirname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const dist = join(root, '.local/web-dist');
const artifacts = join(root, '.local/artifacts');
await mkdir(join(dist, 'probe_pkg'), { recursive: true });
await mkdir(join(dist, 'fixtures'), { recursive: true });
await mkdir(join(dist, 'workers'), { recursive: true });
await mkdir(artifacts, { recursive: true });
execFileSync('cargo', ['run', '--locked', '-p', 'gen', '--example', 'worker_fixture'], { cwd: root, stdio: 'inherit' });
execFileSync('cargo', ['build', '--locked', '-p', 'jobs', '--example', 'browser_probe', '--target', 'wasm32-unknown-unknown'], { cwd: root, stdio: 'inherit' });
execFileSync('wasm-bindgen', ['--target', 'web', '--out-dir', join(dist, 'probe_pkg'), '--out-name', 'browser_probe', join(root, 'target/wasm32-unknown-unknown/debug/examples/browser_probe.wasm')], { cwd: root, stdio: 'inherit' });
for (const name of ['jobs-request.bin', 'jobs-expected.bin']) {
  await writeFile(join(dist, 'fixtures', name), await readFile(join(artifacts, name)));
}
await writeFile(join(dist, 'workers/jobs.js'), await readFile(join(root, 'web/workers/jobs.js')));
await writeFile(join(dist, 'jobs-probe.html'), `<!doctype html><html><body><p id="status">worker contracts</p><script type="module">
import init, {run_probe} from './probe_pkg/browser_probe.js';
try {
  await init();
  const request = new Uint8Array(await (await fetch('./fixtures/jobs-request.bin')).arrayBuffer());
  const expected = new Uint8Array(await (await fetch('./fixtures/jobs-expected.bin')).arrayBuffer());
  const result = await run_probe(new URL('./workers/jobs.js', location.href).href, new URL('./pkg/client_web.js', location.href).href, request, expected);
  window.__JOBS_RESULT = {ok:true, isolated:crossOriginIsolated, ...result};
} catch(error) { window.__JOBS_RESULT = {ok:false, error:String(error?.stack ?? error)}; }
</script></body></html>`);

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function stop(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit').catch(() => {});
  child.kill('SIGTERM');
  await Promise.race([exited, sleep(3000)]);
  if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; }
}

async function run(isolated) {
  const profile = await mkdtemp(join(root, '.local/tmp/jobs-worker-'));
  const server = createServer(async (request, response) => {
    try {
      const path = resolve(dist, '.' + decodeURIComponent(new URL(request.url, 'http://local').pathname));
      const actual = await realpath(path);
      if (!actual.startsWith(dist + sep)) { response.writeHead(403); response.end(); return; }
      response.setHeader('Cache-Control', 'no-store');
      if (isolated) {
        response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
        response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
        response.setHeader('Cross-Origin-Resource-Policy', 'same-origin');
      }
      response.setHeader('Content-Type', actual.endsWith('.wasm') ? 'application/wasm' : actual.endsWith('.js') ? 'text/javascript' : actual.endsWith('.html') ? 'text/html' : 'application/octet-stream');
      response.end(await readFile(actual));
    } catch { response.writeHead(404); response.end(); }
  });
  let browser;
  let socket;
  let browserLog = '';
  const deadline = Date.now() + 45_000;
  try {
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
    browser = spawn(process.env.CHROMIUM ?? 'chromium', ['--headless=new', '--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu', '--no-first-run', '--disable-background-networking', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], {
      cwd: root, stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, TMPDIR: join(root, '.local/tmp') },
    });
    browser.on('error', error => { browserLog += error; });
    browser.stderr.on('data', data => { browserLog = (browserLog + data).slice(-12_000); });
    browser.stdout.on('data', data => { browserLog = (browserLog + data).slice(-12_000); });
    let debugPort;
    while (Date.now() < deadline) {
      try { debugPort = (await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]; if (debugPort) break; } catch { /* startup */ }
      if (!browser.pid || browser.exitCode !== null) throw new Error('Worker test browser failed to start');
      await sleep(50);
    }
    if (!debugPort) throw new Error('Worker test browser DevTools timed out');
    const pages = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
    const page = pages.find(page => page.type === 'page');
    if (!page) throw new Error('Worker test browser has no page');
    socket = new WebSocket(page.webSocketDebuggerUrl);
    await Promise.race([once(socket, 'open'), sleep(5000).then(() => { throw new Error('Worker test DevTools connection timed out'); })]);
    let nextId = 0;
    const pending = new Map();
    const exceptions = [];
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data);
      if (message.id) {
        const callback = pending.get(message.id);
        if (callback) { pending.delete(message.id); message.error ? callback.reject(new Error(JSON.stringify(message.error))) : callback.resolve(message.result); }
      } else if (message.method === 'Runtime.exceptionThrown') exceptions.push(JSON.stringify(message.params));
    });
    function call(method, params = {}) {
      const id = ++nextId;
      return new Promise((resolve, reject) => {
        const timer = setTimeout(() => { pending.delete(id); reject(new Error(`Worker CDP timeout: ${method}`)); }, 5000);
        pending.set(id, { resolve: result => { clearTimeout(timer); resolve(result); }, reject: error => { clearTimeout(timer); reject(error); } });
        socket.send(JSON.stringify({ id, method, params }));
      });
    }
    await call('Runtime.enable');
    await call('Page.enable');
    await call('Page.navigate', { url: `http://127.0.0.1:${server.address().port}/jobs-probe.html` });
    let result;
    while (Date.now() < deadline) {
      const state = await call('Runtime.evaluate', { expression: 'JSON.stringify(window.__JOBS_RESULT)', returnByValue: true });
      if (state.result.value) { result = JSON.parse(state.result.value); break; }
      if (exceptions.length) throw new Error(exceptions.join('\n'));
      await sleep(50);
    }
    if (!result?.ok) throw new Error(`Worker contracts failed: ${JSON.stringify(result)}`);
    const expectedTransport = isolated ? 'shared' : 'transfer';
    if (result.isolated !== isolated || result.transport !== expectedTransport || result.bytesCompared !== 50_883 || result.jobsCompleted !== 5 || !result.cancelled || !result.recovered) throw new Error(`Unexpected worker contract result: ${JSON.stringify(result)}`);
    await writeFile(join(artifacts, `jobs-${expectedTransport}.json`), JSON.stringify(result, null, 2) + '\n');
    console.log(`JOBS_WEB_WORKERS_OK ${JSON.stringify(result)}`);
  } catch (error) {
    if (browserLog) console.error(browserLog);
    throw error;
  } finally {
    socket?.close();
    await stop(browser);
    await new Promise(resolve => server.close(resolve));
    // Chromium helpers can finish a profile write just after the parent exits.
    // Retry that cleanup race; functional errors above still fail the smoke.
    await rm(profile, { recursive: true, force: true, maxRetries: 8, retryDelay: 200 });
  }
}
await run(true);
await run(false);
