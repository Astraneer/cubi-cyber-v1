import test from 'node:test';
import assert from 'node:assert/strict';
import {fixtureOwnerCredential} from './fixture_owner_auth.mjs';
function frame(){
  const name=Buffer.from('owner'),body=Buffer.concat([Buffer.from([0]),Buffer.alloc(16,0x4a),Buffer.from([name.length,0]),name,Buffer.from([1]),Buffer.alloc(32,0x7b)]);
  const header=Buffer.alloc(18);header.write('CUBI');header.writeUInt16LE(1,4);header.writeUInt32LE(body.length,6);
  let checksum=0xcbf29ce484222325n;for(const byte of body)checksum=BigInt.asUintN(64,(checksum^BigInt(byte))*0x100000001b3n);
  header.writeBigUInt64LE(checksum,10);return Buffer.concat([header,body]);
}
test('own valid Hello yields only the existing authentication storage format',()=>{
  const value=fixtureOwnerCredential(frame());assert.equal(value.name,'owner');
  assert.equal(value.storageValue,`${'4a'.repeat(16)}:${'7b'.repeat(32)}`);
});
test('all truncated prefixes and corrupt framing are rejected without byte dumps',()=>{
  const valid=frame();for(let n=0;n<valid.length;n++)assert.throws(()=>fixtureOwnerCredential(valid.subarray(0,n)),/^Error: OwnerIdentity/);
  for(const at of [0,4,6,10,18,35,40]){const bad=Buffer.from(valid);bad[at]^=1;assert.throws(()=>fixtureOwnerCredential(bad),/^Error: OwnerIdentity/);}
});
