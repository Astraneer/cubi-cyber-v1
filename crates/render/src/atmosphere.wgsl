// Original bounded single-scattering atmosphere; distances are kilometres.
// Transmittance is shared by sky, later aerial perspective, water and clouds.
struct Parameters {
    radii:vec4<f32>,
    rayleigh:vec4<f32>,
    mie:vec4<f32>,
    ozone:vec4<f32>,
    sun_eye:vec4<f32>,
    integration:vec4<u32>,
};
@group(0) @binding(0) var<uniform> params:Parameters;
@group(0) @binding(1) var transmittance_out:texture_storage_2d<rgba16float,write>;
@group(0) @binding(2) var transmittance:texture_2d<f32>;
@group(0) @binding(3) var sky_out:texture_storage_2d<rgba16float,write>;

fn extinction(height:f32)->vec3<f32> {
    let h=max(height,0.0);
    let rayleigh=exp(-h/params.radii.z);
    let mie=exp(-h/params.radii.w);
    let ozone=max(0.0,1.0-abs(h-params.ozone.w)/15.0);
    return params.rayleigh.rgb*rayleigh+vec3(params.mie.x+params.mie.y)*mie+params.ozone.rgb*ozone;
}

// x: first physical boundary distance; y: one when the planet blocks the ray.
fn boundary(radius:f32,mu:f32)->vec2<f32> {
    let b=radius*mu;
    let ground_c=(radius-params.radii.x)*(radius+params.radii.x);
    let ground_d=b*b-ground_c;
    if b<0.0&&ground_d>=0.0 {
        return vec2(max(-b-sqrt(ground_d),0.0),1.0);
    }
    let top_c=(radius-params.radii.y)*(radius+params.radii.y);
    return vec2(max(-b+sqrt(max(b*b-top_c,0.0)),0.0),0.0);
}

@compute @workgroup_size(8,8)
fn build_transmittance(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=textureDimensions(transmittance_out);
    if any(id.xy>=size) {return;}
    let uv=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(size);
    let radius=mix(params.radii.x,params.radii.y,uv.y);
    let mu=uv.x*2.0-1.0;
    let ray=boundary(radius,mu);
    if ray.y>0.5 {textureStore(transmittance_out,vec2<i32>(id.xy),vec4(0.0,0.0,0.0,1.0));return;}
    let step=ray.x/f32(params.integration.x);
    var optical_depth=vec3(0.0);
    for (var index=0u;index<params.integration.x;index++) {
        let distance=(f32(index)+0.5)*step;
        let sample_radius=sqrt(max(radius*radius+distance*distance+2.0*radius*mu*distance,0.0));
        optical_depth+=extinction(sample_radius-params.radii.x)*step;
    }
    textureStore(transmittance_out,vec2<i32>(id.xy),vec4(exp(-optical_depth),1.0));
}

fn sunlight(radius:f32,mu:f32)->vec3<f32> {
    // Never filter across the planet silhouette: interpolated nonzero light there
    // would leak through a night-side mountain or the solid planet.
    if boundary(radius,mu).y>0.5 {return vec3(0.0);}
    let size=vec2<i32>(textureDimensions(transmittance));
    let uv=vec2(mu*0.5+0.5,clamp((radius-params.radii.x)/(params.radii.y-params.radii.x),0.0,1.0));
    let point=uv*vec2<f32>(size)-vec2(0.5);
    let low=vec2<i32>(floor(point));let weight=fract(point);
    let a=textureLoad(transmittance,clamp(low,vec2(0),size-vec2(1)),0).rgb;
    let b=textureLoad(transmittance,clamp(low+vec2(1,0),vec2(0),size-vec2(1)),0).rgb;
    let c=textureLoad(transmittance,clamp(low+vec2(0,1),vec2(0),size-vec2(1)),0).rgb;
    let d=textureLoad(transmittance,clamp(low+vec2(1,1),vec2(0),size-vec2(1)),0).rgb;
    return mix(mix(a,b,weight.x),mix(c,d,weight.x),weight.y);
}

@compute @workgroup_size(8,8)
fn build_sky(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=textureDimensions(sky_out);
    if any(id.xy>=size) {return;}
    let uv=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(size);
    let mu=uv.y*2.0-1.0;let azimuth=(uv.x*2.0-1.0)*3.141592653589793;
    let horizontal=sqrt(max(1.0-mu*mu,0.0));
    let direction=vec3(horizontal*cos(azimuth),mu,horizontal*sin(azimuth));
    let origin=vec3(0.0,params.radii.x+params.sun_eye.w,0.0);
    let ray=boundary(origin.y,mu);let step=ray.x/f32(params.integration.y);
    let sun=normalize(params.sun_eye.xyz);
    let cosine=dot(direction,sun);
    let rayleigh_phase=3.0*(1.0+cosine*cosine)/(16.0*3.141592653589793);
    let g=params.mie.z;
    let mie_phase=(1.0-g*g)/(4.0*3.141592653589793*pow(max(1.0+g*g-2.0*g*cosine,0.0001),1.5));
    var optical_depth=vec3(0.0);var radiance=vec3(0.0);
    for (var index=0u;index<params.integration.y;index++) {
        let distance=(f32(index)+0.5)*step;
        let point=origin+direction*distance;let radius=length(point);
        let height=max(radius-params.radii.x,0.0);
        let extinction_step=extinction(height)*step;
        let source=(params.rayleigh.rgb*exp(-height/params.radii.z)*rayleigh_phase
            +vec3(params.mie.x*exp(-height/params.radii.w)*mie_phase))
            *sunlight(radius,dot(point/radius,sun))*params.mie.w;
        // Midpoint transmittance makes the quadrature symmetric in each segment.
        radiance+=exp(-optical_depth-extinction_step*0.5)*source*step;
        optical_depth+=extinction_step;
    }
    // Alpha is diagnostic mean view transmittance, not a replacement for RGB
    // transmittance when the later aerial-perspective pass composites a surface.
    textureStore(sky_out,vec2<i32>(id.xy),vec4(max(radiance,vec3(0.0)),exp(-dot(optical_depth,vec3(1.0/3.0)))));
}
