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
@group(0) @binding(16) var<storage,read> edit_neighbors:array<vec2<u32>>;

struct VertexOut {
    @invariant @builtin(position) position: vec4<f32>,
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
    let entry=visible[bucket * camera.params.y + instance];
    let unit_raster=(camera.params.z&2u)!=0u;
    let q = quads[select(entry,entry>>8u,unit_raster)];
    let object = objects[q.w];
    let bits = q.x;
    var origin = vec3<f32>(f32(bits & 31u), f32((bits >> 5u) & 31u), f32((bits >> 10u) & 31u));
    let face = (bits >> 15u) & 7u;
    var width = f32(((bits >> 18u) & 15u) + 1u);
    var height = f32(((bits >> 22u) & 15u) + 1u);
    let tangent_u = array<vec3<f32>,6>(
        vec3(1.0,0.0,0.0), vec3(1.0,0.0,0.0), vec3(-1.0,0.0,0.0),
        vec3(1.0,0.0,0.0), vec3(0.0,0.0,1.0), vec3(0.0,0.0,-1.0));
    let tangent_v = array<vec3<f32>,6>(
        vec3(0.0,0.0,1.0), vec3(0.0,0.0,-1.0), vec3(0.0,1.0,0.0),
        vec3(0.0,1.0,0.0), vec3(0.0,1.0,0.0), vec3(0.0,1.0,0.0));
    let normals = array<vec3<f32>,6>(
        vec3(0.0,-1.0,0.0), vec3(0.0,1.0,0.0), vec3(0.0,0.0,-1.0),
        vec3(0.0,0.0,1.0), vec3(-1.0,0.0,0.0), vec3(1.0,0.0,0.0));
    if unit_raster {
        let tile=entry&255u;let tile_u=tile%u32(width);let tile_v=tile/u32(width);
        origin+=tangent_u[face]*f32(tile_u)+tangent_v[face]*f32(tile_v);
        width=1.0;height=1.0;
    }
    let uv = vec2<f32>(f32(corner & 1u) * width, f32(corner >> 1u) * height);
    var position = origin + tangent_u[face] * uv.x + tangent_v[face] * uv.y;
    var output: VertexOut;
    let world_position=object.model*vec4(position,1.0);
    output.position = camera.view_projection * world_position;
    output.world_position=world_position.xyz;
    // Cover fixed-point rasterization disagreement at greedy T-junctions.
    // The geometric descriptor and interpolated material coordinates stay exact.
    let neighbour_u=position+tangent_u[face]*width*select(1.0,-1.0,(corner&1u)!=0u);
    let neighbour_v=position+tangent_v[face]*height*select(1.0,-1.0,(corner&2u)!=0u);
    let clip_u=camera.view_projection*(object.model*vec4(neighbour_u,1.0));
    let clip_v=camera.view_projection*(object.model*vec4(neighbour_v,1.0));
    if output.position.w>0.00001&&clip_u.w>0.00001&&clip_v.w>0.00001 {
        let screen=output.position.xy/output.position.w*camera.viewport.xy*0.5;
        let edge_u=screen-clip_u.xy/clip_u.w*camera.viewport.xy*0.5;
        let edge_v=screen-clip_v.xy/clip_v.w*camera.viewport.xy*0.5;
        if length(edge_u)>0.00001&&length(edge_v)>0.00001 {
            var normal_u=normalize(vec2(-edge_v.y,edge_v.x));
            var normal_v=normalize(vec2(-edge_u.y,edge_u.x));
            normal_u*=select(-1.0,1.0,dot(normal_u,edge_u)>0.0);
            normal_v*=select(-1.0,1.0,dot(normal_v,edge_v)>0.0);
            let sum=normal_u+normal_v;
            var offset=sum*(0.00390625/max(dot(sum,normal_u),0.0001));
            offset*=min(1.0,0.125/max(length(offset),0.00001));
            let ndc=output.position.xy/output.position.w+offset*2.0/camera.viewport.xy;
            let delta_u=clip_u-output.position;let delta_v=clip_v-output.position;
            let projected_u=delta_u.xy-ndc*delta_u.w;
            let projected_v=delta_v.xy-ndc*delta_v.w;
            let right=ndc*output.position.w-output.position.xy;
            let determinant=projected_u.x*projected_v.y-projected_u.y*projected_v.x;
            if abs(determinant)>0.0000001 {
                let amount_u=(right.x*projected_v.y-right.y*projected_v.x)/determinant;
                let amount_v=(projected_u.x*right.y-projected_u.y*right.x)/determinant;
                position+=(neighbour_u-position)*amount_u+(neighbour_v-position)*amount_v;
                let expanded_world=object.model*vec4(position,1.0);
                output.position=camera.view_projection*expanded_world;
                output.position.x=ndc.x*output.position.w;
                output.position.y=ndc.y*output.position.w;
                output.world_position=expanded_world.xyz;
            }
        }
    }
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

fn unchanged_page_neighbor(input:VertexOut,point:vec3<u32>,axis:u32,positive:bool)->bool {
    let base=objects[input.object_slot];let row=input.object_slot*7u;
    if edit_neighbors[row].x!=base.generation{return false;}
    let negative_face=array<u32,3>(4u,0u,2u);
    let link=edit_neighbors[row+1u+negative_face[axis]+u32(positive)];
    if link.y==0u{return false;}
    let neighbor=objects[link.x];
    if neighbor.generation!=link.y||(neighbor.enabled&1u)==0u||(neighbor.enabled&2u)!=0u{return false;}
    // Links cannot justify a fringe after one object moves away independently.
    let expected=base.model[3].xyz+base.model[axis].xyz*select(-16.0,16.0,positive);
    if any(abs(neighbor.model[3].xyz-expected)>vec3(0.02)){return false;}
    if any(abs(neighbor.model[0].xyz-base.model[0].xyz)>vec3(0.00001)) ||
       any(abs(neighbor.model[1].xyz-base.model[1].xyz)>vec3(0.00001)) ||
       any(abs(neighbor.model[2].xyz-base.model[2].xyz)>vec3(0.00001)){return false;}
    if (neighbor.enabled&4u)==0u{return true;}
    var owner=point;owner[axis]=select(15u,0u,positive);
    return !owner_masked(link.x,owner);
}

// Explicit axes keep FXC from trying to unroll Naga's guarded while loops.
fn unchanged_axis_neighbor(input:VertexOut,point:vec3<u32>,local:vec3<f32>,uncertainty:vec3<f32>,axis:u32)->bool {
    if abs(input.local_normal[axis])>0.5{return false;}
    // The owner index is clamped at page borders. Preserve the side of
    // that owner even when raster guards put the sample just outside it.
    let fraction=clamp(local[axis]-f32(point[axis]),0.0,1.0);
    if fraction<uncertainty[axis] {
        if point[axis]>0u {var neighbour=point;neighbour[axis]-=1u;if !owner_masked(input.object_slot,neighbour){return true;}}
        else if unchanged_page_neighbor(input,point,axis,false){return true;}
    }
    if 1.0-fraction<uncertainty[axis] {
        if point[axis]<15u {var neighbour=point;neighbour[axis]+=1u;if !owner_masked(input.object_slot,neighbour){return true;}}
        else if unchanged_page_neighbor(input,point,axis,true){return true;}
    }
    return false;
}

fn edited_owner(input:VertexOut)->bool {
    let uncertainty=clamp(fwidth(input.local_position)*0.5,vec3(0.00001),vec3(1.0));
    // Overlay bypasses the base mask. Normal pages with no edits do not fetch it.
    if (input.object_flags&2u)!=0u || (input.object_flags&4u)==0u {return false;}
    let local=input.local_position-input.local_normal*0.0001;
    let point=vec3<u32>(clamp(floor(local),vec3(0.0),vec3(15.0)));
    if !owner_masked(input.object_slot,point) {return false;}
    // The CPU mask includes the full AO radius; unmasked neighbours are unchanged.
    return !(unchanged_axis_neighbor(input,point,local,uncertainty,0u) ||
             unchanged_axis_neighbor(input,point,local,uncertainty,1u) ||
             unchanged_axis_neighbor(input,point,local,uncertainty,2u));
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
