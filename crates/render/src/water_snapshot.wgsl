struct Params{view_projection:mat4x4<f32>,inverse_view_projection:mat4x4<f32>,eye_time:vec4<f32>,absorption:vec4<f32>,scattering:vec4<f32>,optics:vec4<f32>,sun:vec4<f32>,image:vec4<u32>,quality:vec4<u32>,waves:vec4<f32>};
@group(0)@binding(0)var<uniform>params:Params;
@group(0)@binding(1)var opaque_source:texture_multisampled_2d<f32>;
@group(0)@binding(2)var opaque_snapshot:texture_storage_2d<rgba16float,write>;
@compute @workgroup_size(8,8)fn snapshot(@builtin(global_invocation_id)id:vec3<u32>){if any(id.xy>=params.image.xy){return;}var radiance=vec4(0.);for(var i=0;i<4;i++){radiance+=textureLoad(opaque_source,vec2<i32>(id.xy),i)*0.25;}textureStore(opaque_snapshot,vec2<i32>(id.xy),radiance);}
