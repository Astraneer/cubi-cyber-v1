struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Instance{position:vec3<f32>,yaw:f32,codes:vec4<u32>,phase:f32,amount:f32,tint:u32,layer:u32,extra:vec4<u32>};
struct Range{draw:vec4<u32>,minimum:vec4<f32>,maximum:vec4<f32>};
struct Indirect{vertices:u32,count:atomic<u32>,first_vertex:u32,first_instance:u32};
@group(0)@binding(0)var<uniform>camera:Camera;
@group(0)@binding(1)var<storage,read>instances:array<Instance>;
@group(0)@binding(2)var<storage,read_write>visible:array<u32>;
@group(0)@binding(3)var<storage,read_write>indirect:array<Indirect,17>;
@group(0)@binding(4)var<storage,read>ranges:array<Range,17>;
@compute @workgroup_size(1)fn reset(){for(var k=0u;k<17u;k++){indirect[k].vertices=ranges[k].draw.x;indirect[k].first_vertex=ranges[k].draw.y;indirect[k].first_instance=0u;atomicStore(&indirect[k].count,0u);}}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=camera.params.x{return;}let item=instances[id.x];if (item.codes.y&2u)!=0u||(camera.params.z==1u&&(item.codes.y&4u)!=0u){return;}let k=item.codes.x;if k>=17u{return;}let bounds=ranges[k];var left=true;var right=true;var down=true;var up=true;var near=true;var far=true;for(var c=0u;c<8u;c++){let p=select(bounds.minimum.xyz,bounds.maximum.xyz,vec3((c&1u)!=0u,(c&2u)!=0u,(c&4u)!=0u))+item.position;let clip=camera.view_projection*vec4(p,1.);left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;}if left||right||up||down||near||far{return;}let slot=atomicAdd(&indirect[k].count,1u);visible[k*camera.params.y+slot]=id.x;}
