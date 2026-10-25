struct Camera {
    view_projection: mat4x4<f32>,
    viewport: vec4<f32>,
    params: vec4<u32>,
};
struct Object {
    model: mat4x4<f32>,
    bounds_min: vec4<f32>,
    bounds_max: vec4<f32>,
    ranges: array<vec2<u32>,3>,
    generation: u32,
    enabled: u32,
};
struct Indirect {
    vertex_count: u32,
    instance_count: atomic<u32>,
    first_vertex: u32,
    first_instance: u32,
};
@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var<storage,read> objects: array<Object>;
@group(0) @binding(2) var<storage,read_write> visible: array<u32>;
@group(0) @binding(3) var<storage,read_write> indirect: array<Indirect,6>;
@group(0) @binding(4) var hiz: texture_2d<f32>;
@group(0) @binding(5) var<storage,read> edit_masks:array<u32>;
@group(0) @binding(6) var<storage,read> patch_states:array<u32>;
@group(0) @binding(7) var<storage,read> quads:array<vec4<u32>>;

@compute @workgroup_size(1)
fn reset() {
    for (var bucket=0u;bucket<6u;bucket++) {
        indirect[bucket].vertex_count=4u;
        atomicStore(&indirect[bucket].instance_count,0u);
        indirect[bucket].first_vertex=bucket*4u;
        indirect[bucket].first_instance=0u;
    }
}

fn corner(object:Object,index:u32)->vec4<f32> {
    let point=vec3<f32>(
        select(object.bounds_min.x,object.bounds_max.x,(index&1u)!=0u),
        select(object.bounds_min.y,object.bounds_max.y,(index&2u)!=0u),
        select(object.bounds_min.z,object.bounds_max.z,(index&4u)!=0u));
    return camera.view_projection*vec4(point,1.0);
}

fn inside_frustum(object:Object)->bool {
    if (camera.params.z&1u)==0u {return true;}
    var outside_left=true;var outside_right=true;var outside_down=true;
    var outside_up=true;var outside_near=true;var outside_far=true;
    let padding=vec2(2.0)/camera.viewport.xy;
    for (var index=0u;index<8u;index++) {
        let p=corner(object,index);
        outside_left=outside_left&&(p.x < -p.w*(1.0+padding.x));
        outside_right=outside_right&&(p.x > p.w*(1.0+padding.x));
        outside_down=outside_down&&(p.y < -p.w*(1.0+padding.y));
        outside_up=outside_up&&(p.y > p.w*(1.0+padding.y));
        outside_far=outside_far&&(p.z < 0.0);
        outside_near=outside_near&&(p.z > p.w);
    }
    return !(outside_left||outside_right||outside_down||outside_up||outside_far||outside_near);
}

fn hidden(object:Object)->bool {
    var low=vec2<f32>(1e30);var high=vec2<f32>(-1e30);
    var nearest=0.0;
    for (var index=0u;index<8u;index++) {
        let p=corner(object,index);
        // Anything crossing the eye/near plane is conservatively retained.
        if p.w<=0.00001 {return false;}
        let ndc=p.xyz/p.w;
        if ndc.z>1.0 {return false;}
        let uv=vec2<f32>(ndc.x*0.5+0.5,0.5-ndc.y*0.5);
        low=min(low,uv);high=max(high,uv);nearest=max(nearest,ndc.z);
    }
    let viewport=camera.viewport.xy;
    let pixel_low=clamp(floor(low*viewport)-vec2(1.0),vec2(0.0),viewport-vec2(1.0));
    let pixel_high=clamp(ceil(high*viewport)+vec2(1.0),vec2(0.0),viewport-vec2(1.0));
    var span=max(pixel_high.x-pixel_low.x,pixel_high.y-pixel_low.y)+1.0;
    var level=0u;
    loop {
        if span<=2.0 || level+1u>=textureNumLevels(hiz) {break;}
        span=ceil(span*0.5);level++;
    }
    let divisor=f32(1u<<level);
    let first=vec2<i32>(floor(pixel_low/divisor));
    let last=vec2<i32>(floor(pixel_high/divisor));
    var minimum=1.0;
    for (var y=first.y;y<=last.y;y++) {
        for (var x=first.x;x<=last.x;x++) {
            minimum=min(minimum,textureLoad(hiz,vec2<i32>(x,y),i32(level)).r);
        }
    }
    // Reverse-Z minimum and an expanded rectangle never hide an exposed sample.
    // The epsilon also prevents self-occlusion from depth-rounding differences.
    return nearest < minimum-0.0001;
}

fn publish(object:Object,slot:u32) {
    if (object.enabled&8u)!=0u {
        let base_slot=object.ranges[1].x;
        let parent=objects[base_slot];
        if parent.generation!=object.ranges[1].y||(parent.enabled&1u)==0u {return;}
        for(var word=0u;word<128u;word++) {
            var bits=edit_masks[base_slot*128u+word];
            loop {
            if bits==0u {break;}
            let owner=word*32u+firstTrailingBit(bits);
            bits=bits&(bits-1u);
            let state=patch_states[slot*4096u+owner];
            for(var face=0u;face<6u;face++) {
                let face_class=(state>>(face*3u))&7u;
                if face_class==0u {continue;}
                let bucket=face_class-1u;
                let index=atomicAdd(&indirect[bucket].instance_count,1u);
                let descriptor=object.ranges[0].x+owner*6u+face;
                visible[bucket*camera.params.y+index]=select(descriptor,descriptor<<8u,(camera.params.z&2u)!=0u);
            }
            }
        }
        return;
    }
    let masked=(object.enabled&4u)!=0u&&(object.enabled&2u)==0u;
    for (var bucket=0u;bucket<3u;bucket++) {
        let range=object.ranges[bucket];
        let destination=bucket+select(0u,3u,masked);
        if (camera.params.z&2u)==0u {
            let start=atomicAdd(&indirect[destination].instance_count,range.y);
            for (var index=0u;index<range.y;index++) {
                visible[destination*camera.params.y+start+index]=range.x+index;
            }
        } else {
            // CPU-preflighted: <=24576 units/object, and the total allocated
            // object count cannot overflow any one bucket's visible capacity.
            for(var index=0u;index<range.y;index++) {
                let descriptor=range.x+index;let bits=quads[descriptor].x;
                let count=(((bits>>18u)&15u)+1u)*(((bits>>22u)&15u)+1u);
                let start=atomicAdd(&indirect[destination].instance_count,count);
                for(var tile=0u;tile<count;tile++) {
                    visible[destination*camera.params.y+start+tile]=(descriptor<<8u)|tile;
                }
            }
        }
    }
}

@compute @workgroup_size(64)
fn frustum(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=camera.params.x {return;}
    let object=objects[id.x];
    if (object.enabled&1u)==0u || !inside_frustum(object) {return;}
    publish(object,id.x);
}

@compute @workgroup_size(64)
fn occlusion(@builtin(global_invocation_id) id:vec3<u32>) {
    if id.x>=camera.params.x {return;}
    let object=objects[id.x];
    if (object.enabled&1u)==0u || !inside_frustum(object) || hidden(object) {return;}
    publish(object,id.x);
}
