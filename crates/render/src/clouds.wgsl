struct Params {
    inverse_view_projection:mat4x4<f32>,eye_time:vec4<f32>,sun:vec4<f32>,
    layer:vec4<f32>,wind:vec4<f32>,image:vec4<u32>,quality:vec4<u32>,optics:vec4<f32>,
};
struct Stats {steps:atomic<u32>,density:atomic<u32>,pixels:atomic<u32>,active_pixels:atomic<u32>};
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var transmittance:texture_2d<f32>;
@group(0) @binding(2) var sky_view:texture_2d<f32>;
@group(0) @binding(3) var scene_depth:texture_depth_multisampled_2d;
@group(0) @binding(4) var full_cloud:texture_storage_2d<rgba16float,write>;
@group(0) @binding(5) var opaque_cloud:texture_storage_2d<rgba16float,write>;
@group(0) @binding(6) var end_distance:texture_storage_2d<r32float,write>;
@group(0) @binding(7) var<storage,read_write> statistics:Stats;

fn hash(point:vec3<i32>)->f32 {
    var value=bitcast<u32>(point.x)*1664525u+bitcast<u32>(point.y)*1013904223u+bitcast<u32>(point.z)*747796405u;
    value=(value^(value>>16u))*2246822519u;value^=value>>13u;
    return f32(value&16777215u)/16777215.0;
}
fn noise3(point:vec3<f32>)->f32 {
    let low=vec3<i32>(floor(point));let fraction=fract(point);let weight=fraction*fraction*(vec3(3.0)-2.0*fraction);
    var value=0.0;
    for(var corner=0u;corner<8u;corner++){
        let delta=vec3<i32>(i32(corner&1u),i32((corner>>1u)&1u),i32((corner>>2u)&1u));
        let blend=select(vec3(1.0)-weight,weight,delta!=vec3(0));value+=hash(low+delta)*blend.x*blend.y*blend.z;
    }return value;
}
fn noise2(point:vec2<f32>)->f32 {
    let low=vec2<i32>(floor(point));let fraction=fract(point);let weight=fraction*fraction*(vec2(3.0)-2.0*fraction);
    return mix(mix(hash(vec3(low.x,0,low.y)),hash(vec3(low.x+1,0,low.y)),weight.x),mix(hash(vec3(low.x,0,low.y+1)),hash(vec3(low.x+1,0,low.y+1)),weight.x),weight.y);
}
fn density(point:vec3<f32>,volume:bool)->f32 {
    let h=(point.y-params.layer.x)/params.layer.y;
    if h<0.0||h>1.0{return 0.0;}
    if params.wind.w>=0.0{return params.wind.w;}
    let wind=point.xz-params.wind.xy*params.eye_time.w;
    let p=wind*params.wind.z;
    let base=noise2(p)*0.6+noise2(p*2.03+vec2(7.1,3.7))*0.3+noise2(p*4.13+vec2(1.9,8.3))*0.1;
    let coverage=smoothstep(1.0-params.layer.z,1.0-params.layer.z+params.layer.w,base);
    if !volume||coverage==0.0{return coverage;}
    let detail=noise3(vec3(wind.x,point.y,wind.y)*params.wind.z*3.1);
    let height=smoothstep(0.0,0.15,h)*(1.0-smoothstep(0.7,1.0,h));
    return clamp(coverage-0.3*detail+0.1,0.0,1.0)*height;
}
fn lookup(texture:texture_2d<f32>,uv:vec2<f32>)->vec3<f32> {
    let size=vec2<i32>(textureDimensions(texture));let point=uv*vec2<f32>(size)-vec2(0.5);let low=vec2<i32>(floor(point));let weight=fract(point);
    let a=textureLoad(texture,clamp(low,vec2(0),size-vec2(1)),0).rgb;let b=textureLoad(texture,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;
    let c=textureLoad(texture,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;let d=textureLoad(texture,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;
    return mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
}
@compute @workgroup_size(1)
fn reset(){atomicStore(&statistics.steps,0u);atomicStore(&statistics.density,0u);atomicStore(&statistics.pixels,0u);atomicStore(&statistics.active_pixels,0u);}
var<workgroup> counters:array<vec4<u32>,64>;
@compute @workgroup_size(8,8)
fn march(@builtin(global_invocation_id) id:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
    var count=vec4(0u);
    if all(id.xy<params.image.zw) {
        count.z=1u;
        let uv=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(params.image.zw);
        let ndc=vec2(uv.x*2.0-1.0,1.0-uv.y*2.0);
        let far=params.inverse_view_projection*vec4(ndc,0.0,1.0);
        let direction=normalize(far.xyz/far.w-params.eye_time.xyz);
        let pixel=clamp(vec2<i32>(uv*vec2<f32>(params.image.xy)),vec2(0),vec2<i32>(params.image.xy)-vec2(1));
        var closest=0.0;for(var sample=0;sample<4;sample++){closest=max(closest,textureLoad(scene_depth,pixel,sample));}
        var opaque_distance=params.optics.w;
        if closest>0.0{let world=params.inverse_view_projection*vec4(ndc,closest,1.0);opaque_distance=min(length(world.xyz/world.w-params.eye_time.xyz),params.optics.w);}
        var full=vec4(0.0,0.0,0.0,1.0);var opaque=full;
        if abs(direction.y)>0.00001 {
            let a=(params.layer.x-params.eye_time.y)/direction.y;let b=(params.layer.x+params.layer.y-params.eye_time.y)/direction.y;
            let begin=max(min(a,b),0.0);let end=min(max(a,b),params.optics.w);
            if end>begin {
                count.w=1u;let volume=params.quality.z!=0u;
                let steps=select(1u,params.quality.x,volume);let step=(end-begin)/f32(steps);
                let sun=normalize(params.sun.xyz);let cosine=dot(direction,sun);let g=params.optics.y;
                let phase=(1.0-g*g)/(12.5663706144*pow(max(1.0+g*g-2.0*g*cosine,0.0001),1.5));
                let sky=lookup(sky_view,vec2(0.5,0.9))*params.optics.z;
                for(var index=0u;index<steps;index++){
                    let distance=begin+(f32(index)+0.5)*step;let point=params.eye_time.xyz+direction*distance;
                    let cloud=density(point,volume);count.x++;count.y++;
                    var shadow=1.0;
                    if volume&&cloud>0.0&&params.quality.y>0u&&sun.y>0.0 {
                        let sun_path=min((params.layer.x+params.layer.y-point.y)/max(sun.y,0.05),400.0);let light_step=max(sun_path,0.0)/f32(params.quality.y);var optical=0.0;
                        for(var light=0u;light<params.quality.y;light++){optical+=density(point+sun*((f32(light)+0.5)*light_step),true)*light_step;count.y++;}
                        shadow=exp(-optical*params.optics.x);
                    }
                    let sunlight=lookup(transmittance,vec2(sun.y*0.5+0.5,clamp(point.y/100000.0,0.0,1.0)))*params.sun.w*phase*shadow;
                    let incoming=sky+sunlight;let sigma=cloud*params.optics.x;
                    let trans=exp(-sigma*step);full=vec4(full.rgb+full.a*(1.0-trans)*incoming,full.a*trans);
                    let segment=clamp(opaque_distance-(distance-step*0.5),0.0,step);let opaque_trans=exp(-sigma*segment);
                    opaque=vec4(opaque.rgb+opaque.a*(1.0-opaque_trans)*incoming,opaque.a*opaque_trans);
                    if full.a<0.005{break;}
                }
            }
        }
        textureStore(full_cloud,vec2<i32>(id.xy),full);textureStore(opaque_cloud,vec2<i32>(id.xy),opaque);textureStore(end_distance,vec2<i32>(id.xy),vec4(opaque_distance,0.0,0.0,0.0));
    }
    counters[lane]=count;workgroupBarrier();
    if lane==0u{var total=vec4(0u);for(var index=0u;index<64u;index++){total+=counters[index];}atomicAdd(&statistics.steps,total.x);atomicAdd(&statistics.density,total.y);atomicAdd(&statistics.pixels,total.z);atomicAdd(&statistics.active_pixels,total.w);}
}
