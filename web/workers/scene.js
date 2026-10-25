// Optional H1 stress adapter. Production uses jobs.js directly. A timer keeps the
// mesh worker occupied without blocking the UI or spending CPU in a busy loop.
import './jobs.js';
const executeSnapshot = self.onmessage;
const configured = Number(new URL(self.location.href).searchParams.get('meshDelayMs'));
const delayMs = Number.isFinite(configured) ? Math.max(0, Math.min(10000, configured)) : 0;
self.onmessage = async event => {
  const data = event.data;
  if (delayMs && data?.type === 'execute') {
    const validBuffer = data.payload instanceof ArrayBuffer
      || (typeof SharedArrayBuffer === 'function' && data.payload instanceof SharedArrayBuffer);
    // Read only a five-byte view after checking the buffer; malformed array-like
    // values must reach the normal validator without triggering an allocation.
    const bytes = validBuffer && data.payload.byteLength >= 5
      ? new Uint8Array(data.payload, 0, 5) : null;
    // The versioned client SceneTask protocol identifies mesh tasks with tag2.
    if (bytes && bytes[0] === 67 && bytes[1] === 83
      && bytes[2] === 84 && (bytes[3] === 50 || bytes[3] === 51) && bytes[4] === 2) {
      await new Promise(resolve => setTimeout(resolve, delayMs));
    }
  }
  return executeSnapshot(event);
};
