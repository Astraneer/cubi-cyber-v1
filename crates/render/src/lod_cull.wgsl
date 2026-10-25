struct Camera{view_projection:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>};
struct Object{model:mat4x4<f32>,inverse_model:mat4x4<f32>,bounds_min:vec4<f32>,bounds_max:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};
struct Indirect{vertices:u32,count:atomic<u32>,first_vertex:u32,first_instance:u32};
@group(0) @binding(0)var<uniform>camera:Camera;
@group(0) @binding(1)var<storage,read>objects:array<Object>;
@group(0) @binding(2)var<storage,read_write>visible:array<u32>;
@group(0) @binding(3)var<storage,read_write>indirect:Indirect;
@compute @workgroup_size(1)fn reset(){indirect.vertices=36u;atomicStore(&indirect.count,0u);indirect.first_vertex=0u;indirect.first_instance=0u;}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=camera.params.x{return;}let object=objects[id.x];if object.data.w==0u{return;}var left=true;var right=true;var down=true;var up=true;var near=true;var far=true;for(var i=0u;i<8u;i++){let p=select(object.bounds_min.xyz,object.bounds_max.xyz,vec3((i&1u)!=0u,(i&2u)!=0u,(i&4u)!=0u));let clip=camera.view_projection*vec4(p,1.);left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;}if left||right||down||up||near||far{return;}let slot=atomicAdd(&indirect.count,1u);visible[slot]=id.x;}
