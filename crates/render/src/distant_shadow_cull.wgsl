struct Tile{origin:vec3<f32>,step:f32,params:vec4<u32>,minimum:vec4<f32>,maximum:vec4<f32>};
struct Object{model:mat4x4<f32>,inverse_model:mat4x4<f32>,bounds_min:vec4<f32>,bounds_max:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};
struct Indirect{vertices:u32,count:atomic<u32>,first_vertex:u32,first_instance:u32};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>tiles:array<Tile>;
@group(0)@binding(2)var<storage,read>objects:array<Object>;
@group(0)@binding(3)var<storage,read_write>visible:array<u32>;
@group(0)@binding(4)var<storage,read_write>draw:array<Indirect,4>;
@group(0)@binding(6)var<storage,read>quad_payload:array<u32>;
@group(0)@binding(7)var<storage,read>materials:array<vec4<u32>>;
// MODEL_FOOTPRINT binds5 and includes the common old/new reduction record.
fn intersects(minimum:vec3<f32>,maximum:vec3<f32>)->bool{
 var left=true;var right=true;var down=true;var up=true;var near=true;var far=true;
 for(var c=0u;c<8u;c++){
  let p=select(minimum,maximum,vec3((c&1u)!=0u,(c&2u)!=0u,(c&4u)!=0u));
  let clip=camera.view_projection*vec4(p,1.);
  left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;
  up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;
 }
 return !(left||right||down||up||near||far);
}
fn include_bounds(minimum:vec3<f32>,maximum:vec3<f32>){
 // High bits hold actual atlas layers, not a request to collect footprints.
 if (camera.params.w&1u)!=0u {include_model_shadow_bounds(camera.view_projection,f32(camera.params.z),minimum,maximum);}
}
@compute @workgroup_size(1)fn reset(){
 draw[0].vertices=1536u;draw[1].vertices=36u;draw[2].vertices=4u;draw[3].vertices=4u;
 for(var i=0u;i<4u;i++){atomicStore(&draw[i].count,0u);draw[i].first_vertex=0u;draw[i].first_instance=0u;}
}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){
 if id.x<camera.params.x{
  let tile=tiles[id.x];
  if tile.params.x!=0u&&intersects(tile.minimum.xyz,tile.maximum.xyz){
   let at=atomicAdd(&draw[0].count,1u);visible[at]=id.x;include_bounds(tile.minimum.xyz,tile.maximum.xyz);
  }
 }
 if id.x<camera.params.y{
  let object=objects[id.x];
  if (object.data.w&1u)!=0u&&intersects(object.bounds_min.xyz,object.bounds_max.xyz){
   let classes=object.reserved[0].x;let quads=object.reserved[0].z;
   if (classes&1u)!=0u{let at=atomicAdd(&draw[1].count,1u);visible[OPAQUE_START+at]=id.x;}
   // Valid payload spans are disjoint. P+M <= all quads <= this existing
   // pair arena, so head/tail reservations cannot collide under any ordering.
   let capacity=(arrayLength(&visible)-CUTOUT_START)/2u;
   for(var q=0u;q<quads;q++){
    let source=object.reserved[0].y+q*4u;
    if source+1u>=arrayLength(&quad_payload){continue;}
    let material_id=quad_payload[source+1u]&65535u;
    if material_id>=arrayLength(&materials){continue;}
    let material=materials[material_id];
    if material.x>=(camera.params.w>>1u)||(material.y&4u)!=0u{continue;}
    var at=0u;
    if(material.y&2u)!=0u {at=atomicAdd(&draw[3].count,1u);}
    else {at=capacity-1u-atomicAdd(&draw[2].count,1u);}
    visible[CUTOUT_START+at*2u]=id.x;
    visible[CUTOUT_START+at*2u+1u]=source;
   }
   if (classes&1u)!=0u||quads!=0u{include_bounds(object.bounds_min.xyz,object.bounds_max.xyz);}
  }
 }
}
