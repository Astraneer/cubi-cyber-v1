// A height cell has half-open X/Z support and closed surface-height bounds.
// 0: proved outside, 1: proved inside, 2: mixed/unknown proof; use sample test.
fn height_bounds_inside(lo:vec3<f32>,hi:vec3<f32>,a:vec3<f32>,b:vec3<f32>)->bool {
    return all(lo>=a)&&hi.x<=b.x&&hi.z<=b.z&&hi.y<b.y;
}
fn height_bounds_outside(lo:vec3<f32>,hi:vec3<f32>,a:vec3<f32>,b:vec3<f32>)->bool {
    return hi.x<=a.x||lo.x>=b.x||hi.z<=a.z||lo.z>=b.z||hi.y<a.y||lo.y>=b.y;
}
fn height_cell_class(lo:vec3<f32>,hi:vec3<f32>)->u32 {
    let intended_lo=bitcast<vec3<f32>>(near_cover[0].xyz);
    let intended_hi=bitcast<vec3<f32>>(near_cover[1].xyz);
    let intended=near_cover[0].w!=0u;
    if intended&&height_bounds_inside(lo,hi,intended_lo,intended_hi){return 1u;}
    let outside_intended=!intended||height_bounds_outside(lo,hi,intended_lo,intended_hi);
    var outside_extra=true;
    if near_cover[2].w!=0u {
        let base=bitcast<vec3<f32>>(near_cover[2].xyz);
        let end=bitcast<vec3<f32>>(near_cover[3].xyz);
        outside_extra=height_bounds_outside(lo,hi,base,end);
        if !outside_extra {
            let at=floor(((lo+hi)*0.5-base)/16.);
            if all(at>=vec3(0.))&&all(at<vec3(32.)) {
                let page_lo=base+at*16.;
                if height_bounds_inside(lo,hi,page_lo,page_lo+vec3(16.)) {
                    let p=vec3<u32>(at);let index=p.x+32u*(p.z+32u*p.y);
                    let word=4u+index/128u;let lane=(index/32u)%4u;
                    let present=(near_cover[word][lane]&(1u<<(index%32u)))!=0u;
                    if present{return 1u;}
                    outside_extra=true;
                }
            }
        }
    }
    if outside_intended&&outside_extra{return 0u;}
    return 2u;
}
