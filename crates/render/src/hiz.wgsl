@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var destination: texture_storage_2d<r32float,write>;

@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    if any(id.xy>=textureDimensions(destination)) {return;}
    let size=vec2<i32>(textureDimensions(source));
    let p=vec2<i32>(id.xy)*2;
    var minimum=1.0;
    for (var y=0;y<2;y++) {
        for (var x=0;x<2;x++) {
            let position=min(p+vec2<i32>(x,y),size-vec2(1));
            minimum=min(minimum,textureLoad(source,position,0).r);
        }
    }
    textureStore(destination,vec2<i32>(id.xy),vec4<f32>(minimum,0.0,0.0,0.0));
}
