struct Params {
    inverse_view_projection:mat4x4<f32>,eye_time:vec4<f32>,sun:vec4<f32>,
    layer:vec4<f32>,wind:vec4<f32>,image:vec4<u32>,quality:vec4<u32>,optics:vec4<f32>,
};
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var full_cloud:texture_2d<f32>;
@group(0) @binding(2) var opaque_cloud:texture_2d<f32>;
@group(0) @binding(3) var end_distance:texture_2d<f32>;
@group(0) @binding(4) var scene_depth:texture_depth_multisampled_2d;
@group(0) @binding(5) var scene_color:texture_multisampled_2d<f32>;
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position)vec4<f32>{let p=array<vec2<f32>,3>(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));return vec4(p[index],0.0,1.0);}
@fragment fn fragment(@builtin(position) position:vec4<f32>,@builtin(sample_index) sample:u32)->@location(0)vec4<f32>{
    let pixel=clamp(vec2<i32>(position.xy),vec2(0),vec2<i32>(params.image.xy)-vec2(1));
    let source=textureLoad(scene_color,pixel,i32(sample));let depth=textureLoad(scene_depth,pixel,i32(sample));
    let uv=position.xy/vec2<f32>(params.image.xy);let point=uv*vec2<f32>(params.image.zw)-vec2(0.5);let low=vec2<i32>(floor(point));let fraction=fract(point);
    var distance=params.optics.w;
    if depth>0.0{let world=params.inverse_view_projection*vec4(uv.x*2.0-1.0,1.0-uv.y*2.0,depth,1.0);distance=min(length(world.xyz/world.w-params.eye_time.xyz),params.optics.w);}
    var cloud=vec4(0.0);var sum=0.0;
    for(var corner=0u;corner<4u;corner++){
        let offset=vec2<i32>(i32(corner&1u),i32(corner>>1u));let q=clamp(low+offset,vec2(0),vec2<i32>(params.image.zw)-vec2(1));let blend=select(vec2(1.0)-fraction,fraction,offset!=vec2(0));var weight=blend.x*blend.y;
        if depth>0.0{let represented=textureLoad(end_distance,q,0).r;weight*=exp(-abs(represented-distance)/max(0.25,distance*0.02));cloud+=textureLoad(opaque_cloud,q,0)*weight;}else{cloud+=textureLoad(full_cloud,q,0)*weight;}
        sum+=weight;
    }
    if sum<0.00001{return source;}
    cloud/=sum;return vec4(cloud.rgb+source.rgb*cloud.a,source.a);
}
