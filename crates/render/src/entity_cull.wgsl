struct Camera{view_projection:mat4x4<f32>,params:vec4<u32>,reserved:vec4<u32>};
struct Entity{position:vec3<f32>,yaw:f32,angles:vec4<f32>,codes:vec4<u32>,id_high:u32,death_roll:f32,scale:f32,extra:f32};
struct Indirect{vertices:u32,count:atomic<u32>,first_vertex:u32,first_instance:u32};
@group(0) @binding(0)var<uniform>camera:Camera;
@group(0) @binding(1)var<storage,read>entities:array<Entity>;
@group(0) @binding(2)var<storage,read_write>visible:array<u32>;
@group(0) @binding(3)var<storage,read_write>indirect:array<Indirect,8>;
@group(0) @binding(4)var<storage,read>ranges:array<vec4<u32>,8>;
@compute @workgroup_size(1)fn reset(){for(var model=0u;model<8u;model++){indirect[model].vertices=ranges[model].x;atomicStore(&indirect[model].count,0u);indirect[model].first_vertex=ranges[model].y;indirect[model].first_instance=0u;}}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=camera.params.x{return;}let entity=entities[id.x];let kind=entity.codes.x;if kind>=8u{return;}var left=true;var right=true;var down=true;var up=true;var near=true;var far=true;for(var i=0u;i<8u;i++){let offset=vec3(select(-2.2,2.2,(i&1u)!=0u),select(-0.3,2.3,(i&2u)!=0u),select(-2.2,2.2,(i&4u)!=0u))*entity.scale;let clip=camera.view_projection*vec4(entity.position+offset,1.);left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;}if left||right||down||up||near||far{return;}let slot=atomicAdd(&indirect[kind].count,1u);visible[kind*camera.params.y+slot]=id.x;}
