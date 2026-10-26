struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Tile{origin:vec3<f32>,step:f32,params:vec4<u32>,minimum:vec4<f32>,maximum:vec4<f32>};
struct Indirect{vertices:u32,count:atomic<u32>,first_vertex:u32,first_instance:u32};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>tiles:array<Tile>;
@group(0)@binding(2)var<storage,read_write>visible:array<u32>;
@group(0)@binding(3)var<storage,read_write>draw:array<Indirect,2>;
@group(0)@binding(21)var<storage,read>near_cover:array<vec4<u32>>;
fn intersects_near(tile:Tile)->bool {
    if near_cover[0].w!=0u && all(tile.maximum.xyz>=bitcast<vec3<f32>>(near_cover[0].xyz)) && all(tile.minimum.xyz<=bitcast<vec3<f32>>(near_cover[1].xyz)) {return true;}
    return near_cover[2].w!=0u && all(tile.maximum.xyz>=bitcast<vec3<f32>>(near_cover[2].xyz)) && all(tile.minimum.xyz<=bitcast<vec3<f32>>(near_cover[3].xyz));
}
@compute @workgroup_size(1)fn reset(){draw[0].vertices=1536u;atomicStore(&draw[0].count,0u);draw[0].first_vertex=0u;draw[0].first_instance=0u;draw[1].vertices=1536u;atomicStore(&draw[1].count,0u);draw[1].first_vertex=0u;draw[1].first_instance=0u;}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=camera.params.x{return;}let tile=tiles[id.x];if tile.params.x==0u{return;}var left=true;var right=true;var down=true;var up=true;var near=true;var far=true;for(var c=0u;c<8u;c++){let p=select(tile.minimum.xyz,tile.maximum.xyz,vec3((c&1u)!=0u,(c&2u)!=0u,(c&4u)!=0u));let clip=camera.view_projection*vec4(p,1.);left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;}if left||right||down||up||near||far{return;}let bucket=select(0u,1u,intersects_near(tile));let slot=atomicAdd(&draw[bucket].count,1u);visible[slot+bucket*camera.params.x]=id.x;}
