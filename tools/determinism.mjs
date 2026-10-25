#!/usr/bin/env node
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const binary = join(root, 'target', 'debug', process.platform === 'win32' ? 'determinism-runner.exe' : 'determinism-runner');
const native = JSON.parse(execFileSync(binary, [], { encoding: 'utf8', timeout: 120_000 }));
const require = createRequire(import.meta.url);
const wasm = JSON.parse(require(join(root, '.local', 'determinism', 'determinism.js')).run_trace());
assert.equal(native.schema, 1);
assert.deepEqual(native.probe, ['433b5b0ef2856c41', '0e009b2aa4b13193', 'f9b21370b7f50024', '45da32de09dd965a']);
assert.equal(native.ticks.length, 128, 'zero or truncated tick trace is not a test');
assert.deepEqual(wasm.probe, native.probe, 'WASM generator differs from donor reference');
assert.deepEqual(wasm.ticks, native.ticks, 'native/WASM state diverged on a tick');
assert.equal(native.physics.length, 256, 'physics trace did not execute');
assert.deepEqual(wasm.physics, native.physics, 'native/WASM player physics diverged on a tick');
assert.equal(native.entities.length, 256, 'entity trace did not execute');
assert.deepEqual(wasm.entities, native.entities, 'native/WASM AI, mob/item physics, inventory or navigation diverged on a tick');
const directory = join(root, '.local', 'artifacts');
await mkdir(directory, { recursive: true });
await writeFile(join(directory, 'm1-determinism.json'), JSON.stringify({ native, wasm }, null, 2) + '\n');
console.log(`DETERMINISM_OK ticks=${native.ticks.length} physicsTicks=${native.physics.length} entityTicks=${native.entities.length} goldenColumns=${native.probe.length} runtimes=native,wasm final=${native.ticks.at(-1)}`);
