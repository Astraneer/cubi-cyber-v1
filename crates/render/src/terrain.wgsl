struct Camera {
    view_projection: mat4x4<f32>,
    viewport: vec4<f32>,
    params: vec4<u32>,
};
struct Object {
    model: mat4x4<f32>,
    bounds_min: vec4<f32>,
    bounds_max: vec4<f32>,
    ranges: array<vec2<u32>, 3>,
    generation: u32,
    enabled: u32,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<storage, read> objects: array<Object>;
@group(0) @binding(2) var<storage, read> quads: array<vec4<u32>>;
@group(0) @binding(3) var<storage, read> visible: array<u32>;
@group(0) @binding(4) var<storage, read> materials: array<vec4<u32>>;
@group(0) @binding(5) var colors: texture_2d_array<f32>;
@group(0) @binding(6) var tint_masks: texture_2d_array<f32>;
@group(0) @binding(7) var texels: sampler;
@group(0) @binding(8) var<storage,read> edit_masks: array<u32>;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) ao: f32,
    @location(2) @interpolate(flat) normal: vec3<f32>,
    @location(3) @interpolate(flat) material: u32,
    @location(4) @interpolate(flat) tint: vec3<f32>,
    @location(5) @interpolate(flat) bucket: u32,
    @location(6) @interpolate(flat) object_slot: u32,
    @location(7) @interpolate(perspective,centroid) local_position: vec3<f32>,
    @location(8) @interpolate(flat) local_normal: vec3<f32>,
    @location(9) @interpolate(flat) object_flags: u32,
    @location(10) @interpolate(flat) uniform_ao: f32,
    @location(11) world_position:vec3<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) vertex_id: u32, @builtin(instance_index) instance: u32) -> VertexOut {
    let corner = vertex_id % 4u;
    let bucket = vertex_id / 4u;
    let q = quads[visible[bucket * camera.params.y + instance]];
    let object = objects[q.w];
    let bits = q.x;
    let origin = vec3<f32>(f32(bits & 31u), f32((bits >> 5u) & 31u), f32((bits >> 10u) & 31u));
    let face = (bits >> 15u) & 7u;
    let width = f32(((bits >> 18u) & 15u) + 1u);
    let height = f32(((bits >> 22u) & 15u) + 1u);
    let tangent_u = array<vec3<f32>,6>(
        vec3(1.0,0.0,0.0), vec3(1.0,0.0,0.0), vec3(-1.0,0.0,0.0),
        vec3(1.0,0.0,0.0), vec3(0.0,0.0,1.0), vec3(0.0,0.0,-1.0));
    let tangent_v = array<vec3<f32>,6>(
        vec3(0.0,0.0,1.0), vec3(0.0,0.0,-1.0), vec3(0.0,1.0,0.0),
        vec3(0.0,1.0,0.0), vec3(0.0,1.0,0.0), vec3(0.0,1.0,0.0));
    let normals = array<vec3<f32>,6>(
        vec3(0.0,-1.0,0.0), vec3(0.0,1.0,0.0), vec3(0.0,0.0,-1.0),
        vec3(0.0,0.0,1.0), vec3(-1.0,0.0,0.0), vec3(1.0,0.0,0.0));
    let uv = vec2<f32>(f32(corner & 1u) * width, f32(corner >> 1u) * height);
    let position = origin + tangent_u[face] * uv.x + tangent_v[face] * uv.y;
    var output: VertexOut;
    output.position = camera.view_projection * object.model * vec4(position,1.0);
    output.world_position=(object.model*vec4(position,1.0)).xyz;
    let rotation=(q.y>>24u)&3u;
    let anchored_uv=vec2(dot(position,tangent_u[face]),dot(position,tangent_v[face]));
    var texture_uv=anchored_uv;
    switch rotation {
        case 1u: {texture_uv=vec2(anchored_uv.y,1.0-anchored_uv.x);}
        case 2u: {texture_uv=vec2(1.0-anchored_uv.x,1.0-anchored_uv.y);}
        case 3u: {texture_uv=vec2(1.0-anchored_uv.y,anchored_uv.x);}
        default: {}
    }
    output.uv = texture_uv;
    output.ao = 0.55 + 0.15 * f32((q.y >> (16u + corner * 2u)) & 3u);
    output.normal = normalize((object.model * vec4(normals[face],0.0)).xyz);
    output.material = q.y & 65535u;
    let tint = unpack4x8unorm(q.z).rgb;
    output.tint = tint * tint;
    output.bucket = bucket%3u;
    output.object_slot=q.w;
    output.local_position=position;
    output.local_normal=normals[face];
    output.object_flags=object.enabled;
    let first_ao=(q.y>>16u)&3u;
    output.uniform_ao=select(-1.0,0.55+0.15*f32(first_ao),((q.y>>16u)&255u)==first_ao*85u);
    return output;
}

fn owner_masked(slot:u32,point:vec3<u32>)->bool {
    let index=point.x+16u*(point.z+16u*point.y);
    return (edit_masks[slot*128u+index/32u]&(1u<<(index%32u)))!=0u;
}

fn edited_owner(input:VertexOut)->bool {
    let uncertainty=clamp(fwidth(input.local_position)*0.5,vec3(0.00001),vec3(1.0));
    // Overlay bypasses the base mask. Normal pages with no edits do not fetch it.
    if (input.object_flags&2u)!=0u || (input.object_flags&4u)==0u {return false;}
    let local=input.local_position-input.local_normal*0.0001;
    let point=vec3<u32>(clamp(floor(local),vec3(0.0),vec3(15.0)));
    if !owner_masked(input.object_slot,point) {return false;}
    // Keep a conservative fringe of unchanged old geometry. The CPU mask
    // includes the full AO radius, so an unmasked neighbour is unchanged.
    // This covers fixed-point rasterization disagreement at unit-grid edges.
    for (var axis=0u;axis<3u;axis++) {
        if abs(input.local_normal[axis])>0.5 {continue;}
        let fraction=fract(local[axis]);
        if fraction<uncertainty[axis]&&point[axis]>0u {
            var neighbour=point;neighbour[axis]-=1u;
            if !owner_masked(input.object_slot,neighbour) {return false;}
        }
        if 1.0-fraction<uncertainty[axis]&&point[axis]<15u {
            var neighbour=point;neighbour[axis]+=1u;
            if !owner_masked(input.object_slot,neighbour) {return false;}
        }
    }
    return true;
}

@fragment
fn depth_fragment(input:VertexOut) {
    if edited_owner(input) {discard;}
}

@fragment
fn masked_fragment(input: VertexOut) -> @location(0) vec4<f32> {
    let color=shade(input.uv,input.ao,input.normal,input.material,input.tint,input.bucket,input.uniform_ao,input.object_slot,input.local_position,input.local_normal,input.world_position);
    if edited_owner(input) {discard;}
    return color;
}

// Ordinary terrain omits owner-position interpolation and all edit-mask reads.
struct OrdinaryFragmentIn {
    @location(0) uv: vec2<f32>,
    @location(1) ao: f32,
    @location(2) @interpolate(flat) normal: vec3<f32>,
    @location(3) @interpolate(flat) material: u32,
    @location(4) @interpolate(flat) tint: vec3<f32>,
    @location(5) @interpolate(flat) bucket: u32,
    @location(10) @interpolate(flat) uniform_ao:f32,
    @location(6) @interpolate(flat) object_slot:u32,
    @location(7) @interpolate(perspective,centroid) local_position:vec3<f32>,
    @location(8) @interpolate(flat) local_normal:vec3<f32>,
    @location(11) world_position:vec3<f32>,
};
@fragment
fn fragment(input:OrdinaryFragmentIn)->@location(0)vec4<f32> {
    return shade(input.uv,input.ao,input.normal,input.material,input.tint,input.bucket,input.uniform_ao,input.object_slot,input.local_position,input.local_normal,input.world_position);
}

fn surface_light(albedo:vec3<f32>,ao:f32,normal:vec3<f32>,slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>,world_position:vec3<f32>)->vec3<f32> {
    // BEGIN_DEFAULT_LIGHTING
    let sunlight=0.65+0.35*max(dot(normalize(normal),normalize(vec3(0.35,0.8,0.2))),0.0);
    return albedo*sunlight*ao;
    // END_DEFAULT_LIGHTING
}

fn shade(uv:vec2<f32>,ao_vertex:f32,normal:vec3<f32>,material_id:u32,tint:vec3<f32>,bucket:u32,uniform_ao:f32,slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>,world_position:vec3<f32>)->vec4<f32> {
    let material = materials[material_id];
    let layer = i32(material.x);
    let texel = textureSample(colors, texels, uv, layer);
    let mask = textureSample(tint_masks, texels, uv, layer).r;
    let tinted = mix(vec3(1.0), tint, select(1.0,mask,(material.y & 1u)!=0u));
    if bucket==1u && texel.a < 0.001 { discard; }
    // Water is a plain material bucket in M2; physical water is explicitly M10.
    let coverage = select(1.0,texel.a,bucket==1u);
    let ao=select(ao_vertex,uniform_ao,uniform_ao>=0.0);
    return vec4(surface_light(texel.rgb*tinted,ao,normal,slot,local_position,local_normal,world_position),coverage);
}
