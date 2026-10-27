// Own fresh test authority authentication only. Never report the returned value
// or place it in a Runtime expression; use normal DOMStorage before Connect.
export function fixtureOwnerCredential(bytes){
  const check=(value,code)=>{if(!value)throw Error(code);};
  check(Buffer.isBuffer(bytes)&&bytes.length>=70&&bytes.length<=512,'OwnerIdentityBounds');
  check(bytes.toString('ascii',0,4)==='CUBI','OwnerIdentityMagic');
  check(bytes.readUInt16LE(4)===1,'OwnerIdentityVersion');
  check(bytes.readUInt32LE(6)===bytes.length-18,'OwnerIdentityLength');
  let checksum=0xcbf29ce484222325n;
  for(const byte of bytes.subarray(18))checksum=((checksum^BigInt(byte))*0x100000001b3n)&0xffffffffffffffffn;
  check(bytes.readBigUInt64LE(10)===checksum,'OwnerIdentityChecksum');
  let at=18;
  const take=n=>{check(at+n<=bytes.length,'OwnerIdentityTruncated');const part=bytes.subarray(at,at+n);at+=n;return part;};
  check(take(1)[0]===0,'OwnerIdentityHello');
  const identity=take(16);check(identity.some(byte=>byte!==0),'OwnerIdentityAnonymous');
  const length=take(2).readUInt16LE();check(length>0&&length<=64,'OwnerIdentityName');
  let name;try{name=new TextDecoder('utf-8',{fatal:true}).decode(take(length));}catch{throw Error('OwnerIdentityName');}
  check(!/\p{Cc}/u.test(name),'OwnerIdentityName');
  check(take(1)[0]===1,'OwnerIdentityCredential');
  const secret=take(32);check(at===bytes.length,'OwnerIdentityTrailing');
  return {name,identityHex:identity.toString('hex'),storageValue:`${identity.toString('hex')}:${secret.toString('hex')}`};
}
