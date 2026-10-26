// Shared local browser harness for rendering, input, UI and replay acceptance.
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

export const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

export function pngPixels(png) {
  if (!png.subarray(0, 8).equals(Buffer.from([137,80,78,71,13,10,26,10]))) throw new Error('Invalid PNG signature');
  let width, height, channels;
  const compressed = [];
  for (let offset = 8; offset < png.length;) {
    const size = png.readUInt32BE(offset);
    if (offset + size + 12 > png.length) throw new Error('Truncated PNG chunk');
    const type = png.toString('ascii', offset + 4, offset + 8);
    const data = png.subarray(offset + 8, offset + 8 + size);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0); height = data.readUInt32BE(4);
      channels = data[9] === 2 ? 3 : data[9] === 6 ? 4 : 0;
      if (data[8] !== 8 || data[12] !== 0 || !channels) throw new Error('Unsupported PNG encoding');
    }
    if (type === 'IDAT') compressed.push(data);
    offset += size + 12;
  }
  if (!width || !height || width > 4096 || height > 4096) throw new Error('PNG dimensions out of bounds');
  const stride = width * channels;
  const raw = inflateSync(Buffer.concat(compressed), { maxOutputLength: (stride + 1) * height });
  if (raw.length !== (stride + 1) * height) throw new Error('Truncated PNG pixels');
  const pixels = Buffer.alloc(stride * height);
  function paeth(a,b,c) { const p=a+b-c, x=Math.abs(p-a), y=Math.abs(p-b), z=Math.abs(p-c); return x<=y&&x<=z?a:y<=z?b:c; }
  for (let y=0; y<height; y++) {
    const filter = raw[y * (stride + 1)];
    if (filter > 4) throw new Error('Unknown PNG filter');
    for (let x=0; x<stride; x++) {
      const index=y*stride+x, left=x>=channels?pixels[index-channels]:0;
      const up=y?pixels[index-stride]:0, corner=y&&x>=channels?pixels[index-stride-channels]:0;
      const prediction=filter===0?0:filter===1?left:filter===2?up:filter===3?Math.floor((left+up)/2):paeth(left,up,corner);
      pixels[index]=(raw[y*(stride+1)+1+x]+prediction)&255;
    }
  }
  return { width, height, sample(x,y) { const p=(y*width+x)*channels; return [...pixels.subarray(p,p+3),channels===4?pixels[p+3]:255]; } };
}

async function stop(child) {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child,'exit').catch(()=>{});
  child.kill('SIGTERM');
  await Promise.race([exited,pause(2000)]);
  if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; }
}

export async function withBrowser(options, action) {
  const tmp=join(root,'.local','tmp'); await mkdir(tmp,{recursive:true});
  const profile=await mkdtemp(join(tmp,'acceptance-browser-'));
  let server, browser, socket, serverLog='', browserLog='';
  const errors=[], pending=new Map(), listeners=new Map(); let nextId=0;
  try {
    const serverArgs=[process.env.CUBI_WEB_SERVER_SCRIPT??'tools/serve_web.py','--port','0'];
    if(process.env.CUBI_WEB_DIST)serverArgs.push('--directory',process.env.CUBI_WEB_DIST);
    server=spawn(process.env.PYTHON??'python3',serverArgs,{cwd:root,stdio:['ignore','pipe','pipe'],env:{...process.env,TMPDIR:tmp,PYTHONDONTWRITEBYTECODE:'1'}});
    server.on('error',e=>{serverLog+=e;});
    server.stdout.on('data',d=>{serverLog+=d;}); server.stderr.on('data',d=>{serverLog+=d;});
    const startup=Date.now()+30_000; let port;
    while (Date.now()<startup) {
      port=serverLog.match(/WEB_SERVER_PORT=(\d+)/)?.[1]; if(port)break;
      if(server.exitCode!==null||!server.pid)throw new Error(`Server failed: ${serverLog}`);
      await pause(50);
    }
    if(!port)throw new Error('Local server did not become ready');
    browser=spawn(process.env.CHROMIUM??'chromium',[
      ...(options.headed?[]:['--headless=new']),'--no-sandbox','--disable-dev-shm-usage',
      '--no-first-run','--no-default-browser-check','--disable-background-networking',
      '--run-all-compositor-stages-before-draw','--enable-unsafe-webgpu','--enable-unsafe-swiftshader',
      '--ignore-gpu-blocklist','--enable-gpu-rasterization','--use-angle=swiftshader','--use-vulkan=swiftshader',
      '--enable-features=Vulkan','--disable-vulkan-surface','--remote-debugging-port=0',
      `--user-data-dir=${profile}`,`--crash-dumps-dir=${join(profile,'crashes')}`,'about:blank',
    ],{cwd:root,stdio:['ignore','pipe','pipe'],env:{...process.env,TMPDIR:tmp,XDG_CONFIG_HOME:join(root,'.local/config'),XDG_CACHE_HOME:join(root,'.local/cache')}});
    browser.on('error',e=>{browserLog+=e;});
    browser.stdout.on('data',d=>{browserLog=(browserLog+d).slice(-16000);});browser.stderr.on('data',d=>{browserLog=(browserLog+d).slice(-16000);});
    let debugPort;
    while(Date.now()<startup) {
      try { debugPort=(await readFile(join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0]; if(debugPort)break; } catch {}
      if(browser.exitCode!==null||!browser.pid)throw new Error(`Browser failed: ${browserLog}`);
      await pause(50);
    }
    if(!debugPort)throw new Error('Browser did not expose DevTools');
    const targets=await(await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
    const page=targets.find(t=>t.type==='page');if(!page)throw new Error('No browser page');
    socket=new WebSocket(page.webSocketDebuggerUrl);
    await new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>reject(new Error('DevTools connect timed out')),10000);
      socket.addEventListener('open',()=>{clearTimeout(timer);resolve();},{once:true});
      socket.addEventListener('error',()=>{clearTimeout(timer);reject(new Error('DevTools connect failed'));},{once:true});
    });
    socket.addEventListener('message',event=>{
      const message=JSON.parse(event.data);
      if(message.id) { const p=pending.get(message.id);if(!p)return;pending.delete(message.id);clearTimeout(p.timer);if(message.error)p.reject(new Error(JSON.stringify(message.error)));else p.resolve(message.result); }
      else {
        if(message.method==='Runtime.exceptionThrown')errors.push(message.params.exceptionDetails);
        for(const callback of listeners.get(message.method)??[]) callback(message.params);
      }
    });
    function call(method,params={},timeout=30_000) {
      const id=++nextId;return new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>{pending.delete(id);reject(new Error(`CDP timeout: ${method}`));},timeout);
        pending.set(id,{resolve,reject,timer});socket.send(JSON.stringify({id,method,params}));
      });
    }
    async function evaluate(expression) {
      const result=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});
      if(result.exceptionDetails)throw new Error(JSON.stringify(result.exceptionDetails));
      return result.result.value;
    }
    await call('Runtime.enable');await call('Page.enable');
    if(options.width)await call('Emulation.setDeviceMetricsOverride',{width:options.width,height:options.height,deviceScaleFactor:1,mobile:false});
    const url=`http://127.0.0.1:${port}${options.path??'/'}`;
    await call('Page.navigate',{url});
    async function waitFor(expression,predicate,timeout=120_000) {
      const until=Date.now()+timeout;let last;
      while(Date.now()<until) {
        if(errors.length)throw new Error(`Uncaught browser error: ${JSON.stringify(errors)}`);
        last=await evaluate(expression);
        if(predicate(last))return last;
        await pause(100);
      }
      throw new Error(`Browser condition timed out: ${JSON.stringify(last)}`);
    }
    function onEvent(method,callback) {
      if(!listeners.has(method))listeners.set(method,new Set());
      listeners.get(method).add(callback);
      return()=>listeners.get(method)?.delete(callback);
    }
    return await action({call,evaluate,waitFor,onEvent,errors,url});
  } catch(error) {
    if(browserLog)console.error(browserLog);throw error;
  } finally {
    for(const p of pending.values()){clearTimeout(p.timer);p.reject(new Error('Browser closing'));}
    if(socket)socket.close();await stop(browser);await stop(server);
    await rm(profile,{recursive:true,force:true,maxRetries:5,retryDelay:200});
  }
}
