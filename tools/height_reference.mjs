// Independent finite-fixture support. This module receives no shader, guard,
// draw result, Near box, Unknown mask or renderer implementation.
export function heightReferenceSupport(matrix, viewport, offsets, precision, bounds) {
  const [width,height]=viewport,[minX,minZ,maxX,maxZ]=bounds;
  if(!(width>0&&height>0&&minX<maxX&&minZ<maxZ)||!Number.isInteger(precision.subpixelBits))throw Error('Invalid reference domain');
  const project=(x,z)=>{
    const p=[x,8,z,1],clip=[0,1,2,3].map(row=>p.reduce((sum,value,col)=>sum+value*matrix[col][row],0));
    if(!(clip[3]>0))throw Error('Finite support crosses the eye plane; a clipping reference is required');
    return [(clip[0]/clip[3]+1)*width/2,(1-clip[1]/clip[3])*height/2];
  };
  const corners=[project(minX,minZ),project(maxX,minZ),project(maxX,maxZ),project(minX,maxZ)];
  const center=[0,1].map(axis=>corners.reduce((sum,p)=>sum+p[axis],0)/4);
  const step=2**-precision.subpixelBits;
  const roundingError=precision.rounding.startsWith('nearest-')?step/2:step;
  const edges=corners.map((a,index)=>{
    const b=corners[(index+1)%4],delta=[b[0]-a[0],b[1]-a[1]],length=Math.hypot(...delta);
    let normal=[-delta[1]/length,delta[0]/length];
    if(normal[0]*(center[0]-a[0])+normal[1]*(center[1]-a[1])<0)normal=normal.map(x=>-x);
    return {a,b,inwardNormal:normal,maximumSnapErrorPixels:roundingError*(Math.abs(normal[0])+Math.abs(normal[1]))};
  });
  const mask=new Uint8Array(width*height*offsets.length),witnesses=[];
  let outside=0,ambiguous=0,certified=0,fingerprint=2166136261;
  for(let y=0;y<height;y++)for(let x=0;x<width;x++)for(let sample=0;sample<offsets.length;sample++){
    const sx=x+offsets[sample][0],sy=y+offsets[sample][1],nx=2*sx/width-1,ny=1-2*sy/height;
    const a=[matrix[0][0]-nx*matrix[0][3],matrix[2][0]-nx*matrix[2][3]],b=[matrix[0][1]-ny*matrix[0][3],matrix[2][1]-ny*matrix[2][3]];
    const c=[nx*(8*matrix[1][3]+matrix[3][3])-(8*matrix[1][0]+matrix[3][0]),ny*(8*matrix[1][3]+matrix[3][3])-(8*matrix[1][1]+matrix[3][1])];
    const determinant=a[0]*b[1]-a[1]*b[0],world=[(c[0]*b[1]-a[1]*c[1])/determinant,(a[0]*c[1]-c[0]*b[0])/determinant];
    let category=0;
    if(world[0]>=minX&&world[0]<=maxX&&world[1]>=minZ&&world[1]<=maxZ){
      const distances=edges.map(edge=>(sx-edge.a[0])*edge.inwardNormal[0]+(sy-edge.a[1])*edge.inwardNormal[1]);
      category=distances.every((distance,i)=>distance>edges[i].maximumSnapErrorPixels)?2:1;
      if(category===1&&witnesses.length<8)witnesses.push({pixel:[x,y],sample,world,inwardDistances:distances});
    }
    mask[(y*width+x)*offsets.length+sample]=category;
    outside+=Number(category===0);ambiguous+=Number(category===1);certified+=Number(category===2);
    fingerprint=Math.imul(fingerprint^category,16777619)>>>0;
  }
  return {mask,report:{bounds,viewport,samples:offsets.length,precision,corners,edges,outside,ambiguous,certified,fingerprint:fingerprint.toString(16).padStart(8,'0'),witnesses}};
}
