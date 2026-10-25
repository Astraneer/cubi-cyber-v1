@group(0) @binding(0) var source:texture_2d<f32>;
@group(0) @binding(1) var destination:texture_storage_2d<rgba16float,write>;
@group(0) @binding(2) var<uniform> extraction:vec4<f32>;
@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=textureDimensions(destination);if any(id.xy>=size){return;}
    let source_size=vec2<i32>(textureDimensions(source));
    let center=vec2<i32>((vec2<f32>(id.xy)+vec2(0.5))*vec2<f32>(source_size)/vec2<f32>(size));
    var color=vec3(0.0);var weight=0.0;
    for(var y=-1;y<=1;y++){for(var x=-1;x<=1;x++){
        let w=f32(2-abs(x))*f32(2-abs(y));
        color+=textureLoad(source,clamp(center+vec2(x,y),vec2(0),source_size-vec2(1)),0).rgb*w;weight+=w;
    }}
    color=max(color/weight,vec3(0.0));
    if extraction.z>0.5 {
        let brightness=max(color.r,max(color.g,color.b));
        let knee=max(extraction.y,0.0001);
        let soft=clamp(brightness-extraction.x+knee,0.0,2.0*knee);
        let contribution=max(brightness-extraction.x,soft*soft/(4.0*knee))/max(brightness,0.0001);
        color*=max(contribution,0.0);
    }
    textureStore(destination,vec2<i32>(id.xy),vec4(color,1.0));
}
