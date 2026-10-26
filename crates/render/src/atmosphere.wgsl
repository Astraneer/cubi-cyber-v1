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
struct AtmosphereLighting {r:vec4<f32>,g:vec4<f32>,b:vec4<f32>,sun:vec4<f32>,direction:vec4<f32>};
@group(0) @binding(4) var<storage,read_write> lighting:AtmosphereLighting;

fn sample_multiple_scattering(point:vec3<f32>,sun:vec3<f32>,ground_radius:f32,top_radius:f32)->vec3<f32>{return vec3(0.0);}

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

// The shared direct light is evaluated at the actual observer height rather
// than clamped to the first coarse LUT row. This uses the same optical model.
fn solar_transmittance(radius:f32,mu:f32)->vec3<f32> {
    let ray=boundary(radius,mu);if ray.y>0.5{return vec3(0.0);}
    let step=ray.x/f32(params.integration.x);var depth=vec3(0.0);
    for(var sample=0u;sample<params.integration.x;sample++) {
        let distance=(f32(sample)+0.5)*step;
        let r=sqrt(max(radius*radius+distance*distance+2.0*radius*mu*distance,0.0));
        depth+=extinction(r-params.radii.x)*step;
    }
    return exp(-depth);
}
fn sky_radiance(direction:vec3<f32>)->vec4<f32> {
    let origin=vec3(0.0,params.radii.x+params.sun_eye.w,0.0);
    let ray=boundary(origin.y,direction.y);let step=ray.x/f32(params.integration.y);
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
        let sigma_scattering=params.rayleigh.rgb*exp(-height/params.radii.z)+vec3(params.mie.x*exp(-height/params.radii.w));
        let multiple=sigma_scattering*sample_multiple_scattering(point,sun,params.radii.x,params.radii.y)*params.mie.w;
        // Midpoint transmittance makes the quadrature symmetric in each segment.
        radiance+=exp(-optical_depth-extinction_step*0.5)*(source+multiple)*step;
        optical_depth+=extinction_step;
    }
    return vec4(max(radiance,vec3(0.0)),exp(-dot(optical_depth,vec3(1.0/3.0))));
}
// Fixed numerical hemisphere quadrature. Only the first sky invocation does
// these additional sixteen bounded rays; all clients share the resulting light.
const HEMISPHERE=array<vec3<f32>,16>(
    vec3(0.9995115995,0.0312500000,0.0000000000),
    vec3(-0.7341213360,0.0937500000,0.6725152797),
    vec3(0.0863519217,0.1562500000,-0.9839356092),
    vec3(0.5937030420,0.2187500000,0.7743804849),
    vec3(-0.9449650659,0.2812500000,-0.1671510147),
    vec3(0.7923378597,0.3437500000,-0.5040204892),
    vec3(-0.2372165410,0.4062500000,0.8824337087),
    vec3(-0.4071334062,0.4687500000,-0.7839105989),
    vec3(0.7958071926,0.5312500000,0.2906275104),
    vec3(-0.7437742583,0.5937500000,0.3070192017),
    vec3(0.3198102530,0.6562500000,-0.6834159346),
    vec3(0.2080826759,0.7187500000,0.6634003599),
    vec3(-0.5400806368,0.7812500000,-0.3129877685),
    vec3(0.5242174154,0.8437500000,-0.1152477282),
    vec3(-0.2431314596,0.9062500000,0.3458294823),
    vec3(-0.0318756868,0.9687500000,-0.2459824752));
@compute @workgroup_size(8,8)
fn build_sky(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=textureDimensions(sky_out);
    if any(id.xy>=size) {return;}
    let uv=(vec2<f32>(id.xy)+vec2(0.5))/vec2<f32>(size);
    let mu=uv.y*2.0-1.0;let azimuth=(uv.x*2.0-1.0)*3.141592653589793;
    let horizontal=sqrt(max(1.0-mu*mu,0.0));
    let direction=vec3(horizontal*cos(azimuth),mu,horizontal*sin(azimuth));
    textureStore(sky_out,vec2<i32>(id.xy),sky_radiance(direction));
    if all(id.xy==vec2(0u)) {
        var red=vec4(0.0);var green=vec4(0.0);var blue=vec4(0.0);
        for(var sample=0u;sample<16u;sample++) {
            let d=HEMISPHERE[sample];let value=sky_radiance(d).rgb;
            let basis=vec4(0.2820947918,0.4886025119*d.y,0.4886025119*d.z,0.4886025119*d.x);
            // Lambert convolution divided by PI: output is diffuse outgoing
            // radiance for a white receiver, in the sky LUT's physical units.
            let weight=basis*vec4(1.0,0.6666666667,0.6666666667,0.6666666667)*(6.2831853072/16.0);
            red+=weight*value.r;green+=weight*value.g;blue+=weight*value.b;
        }
        lighting.r=red;lighting.g=green;lighting.b=blue;
        let sun=normalize(params.sun_eye.xyz);
        lighting.sun=vec4(solar_transmittance(params.radii.x+params.sun_eye.w,sun.y)*params.mie.w,1.0);
        lighting.direction=vec4(sun,1.0);
    }
}
