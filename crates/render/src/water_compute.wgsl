struct Params{view_projection:mat4x4<f32>,inverse_view_projection:mat4x4<f32>,eye_time:vec4<f32>,absorption:vec4<f32>,scattering:vec4<f32>,optics:vec4<f32>,sun:vec4<f32>,image:vec4<u32>,quality:vec4<u32>,waves:vec4<f32>};
struct Object{model:mat4x4<f32>,metadata:vec4<u32>};
struct Indirect{vertices:u32,count:atomic<u32>,first:u32,first_instance:u32};
@group(0)@binding(0)var<uniform>params:Params;
@group(0)@binding(1)var<storage,read>objects:array<Object>;
@group(0)@binding(2)var<storage,read>quads:array<vec4<u32>>;
@group(0)@binding(3)var<storage,read_write>visible:array<u32>;
@group(0)@binding(4)var<storage,read_write>draw:Indirect;
@compute @workgroup_size(1)fn reset(){draw.vertices=4u;draw.first=0u;draw.first_instance=0u;atomicStore(&draw.count,0u);}
@compute @workgroup_size(64)fn cull(@builtin(global_invocation_id)id:vec3<u32>){if id.x>=params.image.z{return;}let q=quads[id.x];let slot=q.w&65535u;if slot>=params.image.w{return;}let object=objects[slot];if object.metadata.y==0u{return;}if params.quality.w!=0u&&(id.x<object.metadata.z||id.x>=object.metadata.z+object.metadata.w){return;}let origin=vec3<f32>(f32(q.x&31u),f32((q.x>>5u)&31u),f32((q.x>>10u)&31u));var left=true;var right=true;var up=true;var down=true;var near=true;var far=true;for(var c=0u;c<8u;c++){let offset=vec3(select(-17.,17.,(c&1u)!=0u),select(-17.,17.,(c&2u)!=0u),select(-17.,17.,(c&4u)!=0u));let p=object.model*vec4(origin+offset,1.);let clip=params.view_projection*p;left=left&&clip.x < -clip.w;right=right&&clip.x>clip.w;down=down&&clip.y < -clip.w;up=up&&clip.y>clip.w;near=near&&clip.z>clip.w;far=far&&clip.z<0.;}if left||right||up||down||near||far{return;}let index=atomicAdd(&draw.count,1u);visible[index]=id.x;}
