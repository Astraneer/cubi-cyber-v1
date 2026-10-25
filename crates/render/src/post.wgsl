struct Params { exposure:vec4<f32>, adaptation:vec4<f32>, bloom:vec4<f32>, image:vec4<u32> };
@group(0) @binding(0) var hdr:texture_2d<f32>;
@group(0) @binding(1) var<storage,read_write> histogram:array<atomic<u32>,256>;
@group(0) @binding(2) var<storage,read_write> exposure_state:vec4<f32>;
@group(0) @binding(3) var<uniform> params:Params;

@compute @workgroup_size(256)
fn reset(@builtin(local_invocation_index) index:u32) {atomicStore(&histogram[index],0u);}
@compute @workgroup_size(8,8)
fn histogram_luminance(@builtin(global_invocation_id) id:vec3<u32>) {
    let point=id.xy*params.image.z;if any(point>=params.image.xy){return;}
    let color=max(textureLoad(hdr,vec2<i32>(point),0).rgb,vec3(0.0));
    let luminance=max(dot(color,vec3(0.2126,0.7152,0.0722)),0.000244140625);
    let coordinate=clamp((log2(luminance)+12.0)/28.0,0.0,0.999999);
    atomicAdd(&histogram[u32(coordinate*256.0)],1u);
}
@compute @workgroup_size(1)
fn adapt() {
    var count=0u;var logarithm=0.0;
    for(var index=0u;index<256u;index++) {
        let samples=atomicLoad(&histogram[index]);count+=samples;
        logarithm+=(f32(index)+0.5)/256.0*28.0*f32(samples)-12.0*f32(samples);
    }
    let mean=exp2(logarithm/max(f32(count),1.0));
    let exposure_goal=clamp(params.exposure.y/mean,exp2(params.exposure.z),exp2(params.exposure.w));
    let time_constant=select(params.adaptation.z,params.adaptation.y,exposure_goal>exposure_state.x);
    let blend=1.0-exp(-params.adaptation.x/max(time_constant,0.001));
    exposure_state=vec4(mix(exposure_state.x,exposure_goal,blend),exposure_goal,mean,exposure_state.w+1.0);
}
