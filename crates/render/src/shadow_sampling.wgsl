struct ShadowSampling {
    matrices:array<mat4x4<f32>,3>,
    desired_matrices:array<mat4x4<f32>,3>,
    sun:vec4<f32>,
    params:vec4<u32>,
};
@group(0) @binding(9) var<uniform> shadow_sampling:ShadowSampling;
@group(0) @binding(10) var shadow_depth:texture_depth_2d_array;
@group(0) @binding(11) var shadow_compare:sampler_comparison;
// The matrix and depth always share a committed version. Selection follows the
// current footprint so a deferred far cascade cannot move a quality boundary.
// Constant layer indices preserve the same receiver math while avoiding the
// dynamic cascade traversal that removes the D3D12 WARP device (369c8f5 proof).
fn constant_layer_0(world_position:vec3<f32>,bias:f32,normal:vec3<f32>)->vec2<f32> {
    if (shadow_sampling.params.x&(1u<<0u))==0u {return vec2(1.,0.);}
    let p=shadow_sampling.matrices[0u]*vec4(world_position,1.);
    let texel=1./f32(shadow_sampling.params.y);
    if abs(p.x)>1.-2.*texel||abs(p.y)>1.-2.*texel||p.z<0.||p.z>1. {return vec2(1.,0.);}
    // A linear comparison tap covers one texel either side after the 2x2
    // offsets. Bound the receiver-plane depth variation over that footprint;
    // a constant normalized bias alone causes acne on coarse, sloped receivers.
    let m=shadow_sampling.matrices[0u];
    let row_x=vec3(m[0].x,m[1].x,m[2].x);
    let row_y=vec3(m[0].y,m[1].y,m[2].y);
    let row_z=vec3(m[0].z,m[1].z,m[2].z);
    let receiver_slope=(abs(dot(normal,normalize(row_x)))+abs(dot(normal,normalize(row_y))))/max(abs(dot(normal,normalize(row_z))),.05);
    let plane_bias=2.*texel*length(row_z)/length(row_x)*receiver_slope;
    let uv=vec2(p.x*.5+.5,.5-p.y*.5);var visibility=0.;
    for(var y=0;y<2;y++){for(var x=0;x<2;x++){
        let offset=(vec2<f32>(f32(x),f32(y))-vec2(.5))*texel;
        visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+offset,i32(0u),p.z+bias+plane_bias);
    }}
    return vec2(visibility*.25,1.);
}
fn constant_layer_1(world_position:vec3<f32>,bias:f32,normal:vec3<f32>)->vec2<f32> {
    if (shadow_sampling.params.x&(1u<<1u))==0u {return vec2(1.,0.);}
    let p=shadow_sampling.matrices[1u]*vec4(world_position,1.);
    let texel=1./f32(shadow_sampling.params.y);
    if abs(p.x)>1.-2.*texel||abs(p.y)>1.-2.*texel||p.z<0.||p.z>1. {return vec2(1.,0.);}
    // A linear comparison tap covers one texel either side after the 2x2
    // offsets. Bound the receiver-plane depth variation over that footprint;
    // a constant normalized bias alone causes acne on coarse, sloped receivers.
    let m=shadow_sampling.matrices[1u];
    let row_x=vec3(m[0].x,m[1].x,m[2].x);
    let row_y=vec3(m[0].y,m[1].y,m[2].y);
    let row_z=vec3(m[0].z,m[1].z,m[2].z);
    let receiver_slope=(abs(dot(normal,normalize(row_x)))+abs(dot(normal,normalize(row_y))))/max(abs(dot(normal,normalize(row_z))),.05);
    let plane_bias=2.*texel*length(row_z)/length(row_x)*receiver_slope;
    let uv=vec2(p.x*.5+.5,.5-p.y*.5);var visibility=0.;
    for(var y=0;y<2;y++){for(var x=0;x<2;x++){
        let offset=(vec2<f32>(f32(x),f32(y))-vec2(.5))*texel;
        visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+offset,i32(1u),p.z+bias+plane_bias);
    }}
    return vec2(visibility*.25,1.);
}
fn constant_layer_2(world_position:vec3<f32>,bias:f32,normal:vec3<f32>)->vec2<f32> {
    if (shadow_sampling.params.x&(1u<<2u))==0u {return vec2(1.,0.);}
    let p=shadow_sampling.matrices[2u]*vec4(world_position,1.);
    let texel=1./f32(shadow_sampling.params.y);
    if abs(p.x)>1.-2.*texel||abs(p.y)>1.-2.*texel||p.z<0.||p.z>1. {return vec2(1.,0.);}
    // A linear comparison tap covers one texel either side after the 2x2
    // offsets. Bound the receiver-plane depth variation over that footprint;
    // a constant normalized bias alone causes acne on coarse, sloped receivers.
    let m=shadow_sampling.matrices[2u];
    let row_x=vec3(m[0].x,m[1].x,m[2].x);
    let row_y=vec3(m[0].y,m[1].y,m[2].y);
    let row_z=vec3(m[0].z,m[1].z,m[2].z);
    let receiver_slope=(abs(dot(normal,normalize(row_x)))+abs(dot(normal,normalize(row_y))))/max(abs(dot(normal,normalize(row_z))),.05);
    let plane_bias=2.*texel*length(row_z)/length(row_x)*receiver_slope;
    let uv=vec2(p.x*.5+.5,.5-p.y*.5);var visibility=0.;
    for(var y=0;y<2;y++){for(var x=0;x<2;x++){
        let offset=(vec2<f32>(f32(x),f32(y))-vec2(.5))*texel;
        visibility+=textureSampleCompareLevel(shadow_depth,shadow_compare,uv+offset,i32(2u),p.z+bias+plane_bias);
    }}
    return vec2(visibility*.25,1.);
}
fn shadow_layer(cascade:u32,world_position:vec3<f32>,bias:f32,normal:vec3<f32>)->vec2<f32> {
    switch cascade {
        case 0u: {return constant_layer_0(world_position,bias,normal);}
        case 1u: {return constant_layer_1(world_position,bias,normal);}
        case 2u: {return constant_layer_2(world_position,bias,normal);}
        default: {return vec2(1.,0.);}
    }
}
fn cascade_shadow(world_position:vec3<f32>,world_normal:vec3<f32>)->f32 {
    let direction=normalize(shadow_sampling.sun.xyz);
    let bias=shadow_sampling.sun.w*(1.+2.*(1.-max(dot(world_normal,direction),0.)));
    // A valid selected layer and at most one valid farther layer sample PCF:
    // four comparisons each, preserving the eight-comparison upper bound.
    if (shadow_sampling.params.x&1u)!=0u {
        let p=shadow_sampling.desired_matrices[0u]*vec4(world_position,1.);
        let edge=max(abs(p.x),abs(p.y));
        if !(edge>=.96||p.z<0.||p.z>1.) {
            let here=constant_layer_0(world_position,bias,world_normal);
            if here.y!=0. {
                if edge>.60 {
                    let next1=constant_layer_1(world_position,bias,world_normal);
                    if next1.y>0. {return mix(here.x,next1.x,smoothstep(.60,.96,edge));}
                    let next2=constant_layer_2(world_position,bias,world_normal);
                    if next2.y>0. {return mix(here.x,next2.x,smoothstep(.60,.96,edge));}
                }
                return here.x;
            }
        }
    }
    if (shadow_sampling.params.x&2u)!=0u {
        let p=shadow_sampling.desired_matrices[1u]*vec4(world_position,1.);
        let edge=max(abs(p.x),abs(p.y));
        if !(edge>=.96||p.z<0.||p.z>1.) {
            let here=constant_layer_1(world_position,bias,world_normal);
            if here.y!=0. {
                if edge>.60 {
                    let next2=constant_layer_2(world_position,bias,world_normal);
                    if next2.y>0. {return mix(here.x,next2.x,smoothstep(.60,.96,edge));}
                }
                return here.x;
            }
        }
    }
    if (shadow_sampling.params.x&4u)!=0u {
        let p=shadow_sampling.desired_matrices[2u]*vec4(world_position,1.);
        let edge=max(abs(p.x),abs(p.y));
        if !(edge>=.96||p.z<0.||p.z>1.) {
            let here=constant_layer_2(world_position,bias,world_normal);
            if here.y!=0. {return here.x;}
        }
    }
    return 1.;
}
