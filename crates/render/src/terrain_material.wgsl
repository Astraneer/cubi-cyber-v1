struct MaterialControl {eye:vec4<f32>,params:vec4<u32>};
@group(0) @binding(17) var material_normals:texture_2d_array<f32>;
@group(0) @binding(18) var material_specs:texture_2d_array<f32>;
@group(0) @binding(19) var<uniform> material_control:MaterialControl;
fn material_f0(albedo:vec3<f32>,pbr:LabPbrSample)->vec3<f32>{
    // Physical approximations with provenance in docs/M10-materials.md. These
    // are deliberately not described as the canonical LabPBR conductor table.
    switch pbr.metal_id {
        case 230u:{return vec3(0.554221760,0.570481595,0.570481595);}
        case 231u:{return vec3(1.0,0.692071057,0.284646690);}
        case 232u:{return vec3(0.807345909,0.827570830,0.827570830);}
        case 233u:{return vec3(0.643027346,0.651117231,0.662150269);}
        case 234u:{return vec3(0.933106842,0.507078510,0.342391640);}
        case 235u:{return vec3(0.906284547,0.923337095,0.929386526);}
        case 236u:{return vec3(0.655930683,0.620915862,0.570481595);}
        case 237u:{return vec3(0.933106842,0.911407500,0.807345909);}
        case 255u:{return albedo;}
        default:{}
    }
    return vec3(pbr.dielectric_f0);
}
fn material_brdf(albedo:vec3<f32>,pbr:LabPbrSample,n:vec3<f32>,v:vec3<f32>,l:vec3<f32>)->vec3<f32>{
    let ndl=max(dot(n,l),0.0);let ndv=max(dot(n,v),0.0001);
    if ndl<=0.0{return vec3(0.0);}
    let half=v+l;let h=half/max(length(half),0.0001);let ndh=max(dot(n,h),0.0);let vdh=max(dot(v,h),0.0);
    let alpha=max(pbr.roughness,0.045);let a2=alpha*alpha;
    let denominator=ndh*ndh*(a2-1.0)+1.0;
    let distribution=a2/max(3.14159265359*denominator*denominator,0.000001);
    let k=(alpha+1.0)*(alpha+1.0)*0.125;
    let masking=ndl/(ndl*(1.0-k)+k)*ndv/(ndv*(1.0-k)+k);
    let f0=material_f0(albedo,pbr);let fresnel=f0+(vec3(1.0)-f0)*pow(1.0-vdh,5.0);
    let specular=fresnel*distribution*masking/max(4.0*ndl*ndv,0.0001);
    let diffuse=albedo*(vec3(1.0)-fresnel)*select(1.0,0.0,pbr.metal_id>=230u)*0.3183098862;
    return (diffuse+specular)*ndl;
}
fn shade(uv:vec2<f32>,ao_vertex:f32,normal:vec3<f32>,material_id:u32,tint:vec3<f32>,bucket:u32,uniform_ao:f32,slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>,world_position:vec3<f32>,tangent_u:vec3<f32>,tangent_v:vec3<f32>)->vec4<f32>{
    if material_control.params.x==0u{return legacy_shade(uv,ao_vertex,normal,material_id,tint,bucket,uniform_ao,slot,local_position,local_normal,world_position);}
    let material=materials[material_id];let layer=i32(material.x);
    let delta=material_control.eye.xyz-world_position;let distance=length(delta);
    let view=select(normal,delta/max(distance,0.0001),distance>0.0001);
    let view_dx=vec3(dot(view,tangent_u),dot(view,tangent_v),dot(view,normal));
    let adjusted=labpbr_parallax_uv(uv,view_dx,distance,0.06,material_control.params.y,material_normals,texels,layer);
    let texel=textureSample(colors,texels,adjusted,layer);
    let mask=textureSample(tint_masks,texels,adjusted,layer).r;
    let tinted=mix(vec3(1.0),tint,select(1.0,mask,(material.y&1u)!=0u));
    if bucket==1u&&texel.a<0.001{discard;}
    let coverage=select(1.0,texel.a,bucket==1u);
    var pbr=decode_labpbr(textureSample(material_normals,texels,adjusted,layer),textureSample(material_specs,texels,adjusted,layer));
    if (material.z&2u)!=0u{pbr.emission=1.0;}
    let mapped_normal=labpbr_world_normal(pbr,tangent_u,tangent_v,normal);
    let ao=select(ao_vertex,uniform_ao,uniform_ao>=0.0)*pbr.ao;
    return vec4(pack_light(texel.rgb*tinted,pbr,ao,mapped_normal,view,slot,local_position,local_normal,world_position),coverage);
}
