@group(0) @binding(0) var depth: texture_depth_multisampled_2d;
@group(0) @binding(1) var base: texture_storage_2d<r32float,write>;

@compute @workgroup_size(8,8)
fn main(@builtin(global_invocation_id) id:vec3<u32>) {
    let output_size=textureDimensions(base);
    if any(id.xy>=output_size) {return;}
    let depth_size=textureDimensions(depth);
    var minimum=0.0;
    if all(id.xy<depth_size) {
        minimum=1.0;
        for (var sample=0u;sample<4u;sample++) {
            minimum=min(minimum,textureLoad(depth,vec2<i32>(id.xy),i32(sample)));
        }
    }
    // Padded pixels and uncovered MSAA samples are zero/far: no false occluders.
    textureStore(base,vec2<i32>(id.xy),vec4<f32>(minimum,0.0,0.0,0.0));
}
