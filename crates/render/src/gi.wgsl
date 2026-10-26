// Original bounded voxel DDA. Only reflected surface radiance enters probes;
// escaped sky and direct emission are deliberately not counted a second time.
struct Params {
    origin_spacing:vec4<f32>, probe_counts:vec4<u32>,
    world_origin:vec4<i32>, world_counts:vec4<u32>,
    trace:vec4<f32>, stamps:vec4<u32>,
};
struct Probe {
    position:vec4<f32>,sh_r:vec4<f32>,sh_g:vec4<f32>,sh_b:vec4<f32>,
    moments:vec4<f32>,visibility_a:vec4<f32>,visibility_b:vec4<f32>,stamps:vec4<u32>,
};
struct Job {slot:u32,generation:u32,revision:u32,rays:u32};
struct Statistics {steps:atomic<u32>,hits:atomic<u32>,unknown:atomic<u32>,published:atomic<u32>};
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var<storage,read> world:array<u32>;
@group(0) @binding(2) var<storage,read_write> probes:array<Probe>;
@group(0) @binding(3) var<storage,read> jobs:array<Job>;
@group(0) @binding(4) var<storage,read> directions:array<vec4<f32>,64>;
@group(0) @binding(5) var<storage,read_write> statistics:Statistics;

fn voxel_index(cell:vec3<i32>)->u32 {
    let page=(cell>>vec3(4u))-params.world_origin.xyz;
    if any(page<vec3(0))||any(vec3<u32>(page)>=params.world_counts.xyz){return 0xffffffffu;}
    let counts=params.world_counts.xyz;let ring_index=params.world_counts.w;
    let ring=vec3(ring_index%counts.x,ring_index/(counts.x*counts.z),ring_index/counts.x%counts.z);
    let p=(vec3<u32>(page)+ring)%counts;let local=vec3<u32>(cell)&vec3(15u);
    let page_index=p.x+params.world_counts.x*(p.z+params.world_counts.z*p.y);
    return page_index*4096u+local.x+16u*(local.z+16u*local.y);
}
fn voxel(cell:vec3<i32>)->u32 {
    let index=voxel_index(cell);if index==0xffffffffu{return 0u;}return world[index];
}
struct Ray {radiance:vec3<f32>,distance:f32,steps:u32,hit:u32,unknown:u32};
fn trace_ray(origin:vec3<f32>,direction:vec3<f32>,evaluate_radiance:bool)->Ray {
    var cell=vec3<i32>(floor(origin));let delta=abs(1.0/select(direction,vec3(1e-20),abs(direction)<vec3(1e-20)));
    let advance=vec3<i32>(sign(direction));
    let boundary=vec3<f32>(cell)+select(vec3(0.0),vec3(1.0),direction>vec3(0.0));
    var next=(boundary-origin)/select(direction,vec3(1e-20),abs(direction)<vec3(1e-20));
    next=select(next,vec3(1e30),abs(direction)<vec3(1e-20));
    var distance=0.0;var steps=0u;var normal=vec3(0.0);
    for(var iteration=0u;iteration<params.stamps.y;iteration++) {
        steps++;
        let packed=voxel(cell);
        if (packed&0x80000000u)==0u{return Ray(vec3(0.0),distance,steps,0u,1u);}
        if (packed&0x40000000u)!=0u {
            var color=vec3(0.0);if evaluate_radiance {color=surface_radiance(voxel_index(cell),packed,normal,origin+direction*distance);}
            return Ray(color,distance,steps,1u,0u);
        }
        distance=min(next.x,min(next.y,next.z));
        if distance>=params.trace.x{return Ray(vec3(0.0),params.trace.x,steps,0u,0u);}
        let crossing=next<=vec3(distance+0.00001);
        // Component assignments avoid a dynamically indexed local-vector
        // l-value, which FXC cannot address inside the bounded DDA loop.
        // Preserve the original X, then Y, then Z priority at exact ties.
        if next.x<=next.y&&next.x<=next.z{normal=vec3(-f32(advance.x),0.,0.);}
        else if next.y<=next.z{normal=vec3(0.,-f32(advance.y),0.);}
        else{normal=vec3(0.,0.,-f32(advance.z));}
        cell+=select(vec3(0),advance,crossing);next+=select(vec3(0.0),delta,crossing);
    }
    // Budget exhaustion is dark, with a conservative visibility boundary.
    return Ray(vec3(0.0),distance,steps,0u,0u);
}
fn account(ray:Ray) {
    atomicAdd(&statistics.steps,ray.steps);atomicAdd(&statistics.hits,ray.hit);atomicAdd(&statistics.unknown,ray.unknown);
}
@compute @workgroup_size(1)
fn reset_statistics() {
    atomicStore(&statistics.steps,0u);atomicStore(&statistics.hits,0u);atomicStore(&statistics.unknown,0u);atomicStore(&statistics.published,0u);
}
var<workgroup> red:array<vec4<f32>,64>;
var<workgroup> green:array<vec4<f32>,64>;
var<workgroup> blue:array<vec4<f32>,64>;
var<workgroup> distances:array<f32,64>;
var<workgroup> axial:array<f32,6>;
@compute @workgroup_size(64)
fn trace_probes(@builtin(workgroup_id) workgroup:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
    let job=jobs[workgroup.x];let probe=probes[job.slot];
    let valid=job.generation==probe.stamps.x&&job.revision==params.stamps.x&&(voxel(vec3<i32>(floor(probe.position.xyz)))&0xc0000000u)==0x80000000u;
    red[lane]=vec4(0.0);green[lane]=vec4(0.0);blue[lane]=vec4(0.0);distances[lane]=0.0;
    if valid&&lane<job.rays {
        let direction=directions[lane*64u/job.rays].xyz;
        let ray=trace_ray(probe.position.xyz,direction,true);account(ray);
        let basis=vec4(0.2820947918,0.4886025119*direction.y,0.4886025119*direction.z,0.4886025119*direction.x);
        let convolution=vec4(3.1415926536,2.0943951024,2.0943951024,2.0943951024);
        let scale=basis*convolution*(12.5663706144/f32(job.rays));
        red[lane]=scale*ray.radiance.r;green[lane]=scale*ray.radiance.g;blue[lane]=scale*ray.radiance.b;distances[lane]=ray.distance;
    }
    if lane<6u {
        axial[lane]=0.0;
        if valid {
            let axis=array<vec3<f32>,6>(vec3(-1.0,0.0,0.0),vec3(1.0,0.0,0.0),vec3(0.0,-1.0,0.0),vec3(0.0,1.0,0.0),vec3(0.0,0.0,-1.0),vec3(0.0,0.0,1.0));
            let ray=trace_ray(probe.position.xyz,axis[lane],false);account(ray);axial[lane]=ray.distance;
        }
    }
    workgroupBarrier();
    if lane==0u {
        var result=probe;result.sh_r=vec4(0.0);result.sh_g=vec4(0.0);result.sh_b=vec4(0.0);
        var mean=0.0;var second=0.0;
        for(var ray=0u;ray<job.rays;ray++){result.sh_r+=red[ray];result.sh_g+=green[ray];result.sh_b+=blue[ray];mean+=distances[ray];second+=distances[ray]*distances[ray];}
        result.moments=vec4(mean/f32(job.rays),second/f32(job.rays),params.trace.x,0.0);
        result.visibility_a=vec4(axial[0],axial[1],axial[2],axial[3]);result.visibility_b=vec4(axial[4],axial[5],0.0,0.0);
        result.stamps=vec4(job.generation,job.revision,u32(valid),job.rays);
        // An obsolete job never replaces a current generation/source result.
        if job.generation==probe.stamps.x&&job.revision==params.stamps.x{probes[job.slot]=result;atomicAdd(&statistics.published,1u);}
    }
}
