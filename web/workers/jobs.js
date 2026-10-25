// Generic snapshot executor. It never owns or mutates the authoritative world.
// Rust Scheduler validates job identity and captured versions before publication.
let execute;
let initialized = false;
let busy = false;

function errorText(error) {
  return String(error?.message ?? error).slice(0, 4096);
}

self.onmessage = async ({ data }) => {
  if (data?.type === 'init') {
    try {
      if (initialized) throw new Error('Worker initialization already started');
      initialized = true;
      const url = new URL(data.module, self.location.href);
      if (url.origin !== self.location.origin || !/^https?:$/.test(url.protocol)) {
        throw new Error('Snapshot module must be served from the same origin');
      }
      if (typeof data.export !== 'string' || !/^[A-Za-z_][A-Za-z0-9_]*$/.test(data.export)) {
        throw new Error('Invalid snapshot export name');
      }
      const module = await import(url.href);
      if (typeof module.default === 'function') await module.default();
      execute = module[data.export];
      if (typeof execute !== 'function') throw new Error('Snapshot export is unavailable');
      self.postMessage({ type: 'ready' });
    } catch (error) {
      self.postMessage({ type: 'error', error: errorText(error) });
    }
    return;
  }
  if (data?.type !== 'execute') return;
  const id = data.id;
  let ownsExecution = false;
  try {
    if (!execute || busy) throw new Error('Snapshot worker is not ready');
    if (typeof id !== 'string' || !/^[1-9][0-9]*$/.test(id)) throw new Error('Invalid job identity');
    if (!Number.isSafeInteger(data.outputLimit) || data.outputLimit < 0) throw new Error('Invalid output byte limit');
    const shared = data.transport === 'shared';
    if (data.transport !== 'transfer' && !shared) throw new Error('Invalid transport');
    const sharedAvailable = typeof SharedArrayBuffer === 'function' && self.crossOriginIsolated;
    if (shared && (!sharedAvailable || !(data.payload instanceof SharedArrayBuffer) || !(data.control instanceof SharedArrayBuffer) || data.control.byteLength !== 4)) {
      throw new Error('Shared transport requires isolation and valid shared buffers');
    }
    if (!shared && !(data.payload instanceof ArrayBuffer)) throw new Error('Transfer transport requires an ArrayBuffer');
    const control = shared ? new Int32Array(data.control) : null;
    if (control && Atomics.load(control, 0) !== 0) { self.postMessage({ type: 'cancelled', id }); return; }
    busy = true;
    ownsExecution = true;
    const result = await execute(new Uint8Array(data.payload), control);
    if (control && Atomics.load(control, 0) !== 0) { self.postMessage({ type: 'cancelled', id }); return; }
    if (!(result instanceof Uint8Array) || result.byteLength > data.outputLimit) throw new Error('Snapshot result violates its byte limit');
    if (shared) {
      const payload = new SharedArrayBuffer(result.byteLength);
      new Uint8Array(payload).set(result);
      self.postMessage({ type: 'result', id, payload });
    } else {
      // Own the returned bytes: a WASM-backed view must never detach its module memory.
      const payload = result.slice().buffer;
      self.postMessage({ type: 'result', id, payload }, [payload]);
    }
  } catch (error) {
    self.postMessage({ type: 'error', id, error: errorText(error) });
  } finally {
    if (ownsExecution) busy = false;
  }
};
