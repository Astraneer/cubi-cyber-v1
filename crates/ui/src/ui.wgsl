struct Viewport {size:vec2<f32>,encode_srgb:f32,padding:f32};
@group(0) @binding(0) var<uniform> viewport:Viewport;
@group(0) @binding(1) var atlas:texture_2d<f32>;
@group(0) @binding(2) var pixel_sampler:sampler;
struct VertexOutput {@builtin(position) position:vec4<f32>,@location(0) uv:vec2<f32>,@location(1) color:vec4<f32>};
fn decode_srgb(v:vec3<f32>)->vec3<f32>{return select(pow((v+vec3<f32>(0.055))/1.055,vec3<f32>(2.4)),v/12.92,v<=vec3<f32>(0.04045));}
fn encode_srgb(v:vec3<f32>)->vec3<f32>{return select(1.055*pow(max(v,vec3<f32>(0.0)),vec3<f32>(1.0/2.4))-0.055,v*12.92,v<=vec3<f32>(0.0031308));}
@vertex fn vertex(@location(0) position:vec2<f32>,@location(1) uv:vec2<f32>,@location(2) color:vec4<f32>)->VertexOutput {var result:VertexOutput;result.position=vec4<f32>(position.x/viewport.size.x*2.0-1.0,1.0-position.y/viewport.size.y*2.0,0.0,1.0);result.uv=uv;result.color=vec4<f32>(decode_srgb(color.rgb),color.a);return result;}
@fragment fn fragment(input:VertexOutput)->@location(0) vec4<f32>{let texel=textureSample(atlas,pixel_sampler,input.uv);var color=texel.rgb*input.color.rgb;if viewport.encode_srgb>0.5 {color=encode_srgb(color);}return vec4<f32>(color,texel.a*input.color.a);}
