// Public machine modes are packed in WORLD Face6 order, three bits per side.
// The high presence bit separates an absent hint from a confirmed all-None hint.
fn machine_port_mode(word:u32,normal:vec3<f32>)->u32 {
    if (word&0x80000000u)==0u {return 7u;}
    var face=7u;
    if normal.y < -.9999 {face=0u;}else if normal.y>.9999 {face=1u;}
    else if normal.z < -.9999 {face=2u;}else if normal.z>.9999 {face=3u;}
    else if normal.x < -.9999 {face=4u;}else if normal.x>.9999 {face=5u;}
    if face==7u {return 7u;}
    return (word>>(face*3u))&7u;
}
fn machine_port_linear(mode:u32)->vec3<f32> {
    // EnumColor from whitelisted MEK-08-09 section1.5. No UI HSV shift.
    let srgb=array<vec3<f32>,6>(vec3(207.,207.,207.),vec3(201.,7.,31.),
        vec3(54.,107.,208.),vec3(164.,96.,217.),vec3(89.,193.,95.),vec3(255.,221.,79.))[min(mode,5u)]/255.;
    return select(pow((srgb+vec3(.055))/1.055,vec3(2.4)),srgb/12.92,srgb<=vec3(.04045));
}
fn machine_port_overlay(albedo:vec3<f32>,mode:u32,uv:vec2<f32>)->vec3<f32> {
    if mode>5u||any(abs(uv-vec2(.5))>vec2(.18)) {return albedo;}
    // An albedo annotation receives the same certified light/shadow as its face.
    return mix(albedo,machine_port_linear(mode),.6);
}

fn connector_linear(color:u32)->vec3<f32>{
 let srgb=array<vec3<f32>,18>(vec3(64.,64.,64.),vec3(54.,107.,208.),vec3(89.,193.,95.),vec3(0.,243.,208.),vec3(201.,7.,31.),vec3(164.,96.,217.),vec3(255.,161.,96.),vec3(207.,207.,207.),vec3(122.,122.,122.),vec3(85.,158.,255.),vec3(117.,255.,137.),vec3(48.,255.,249.),vec3(255.,56.,60.),vec3(213.,94.,203.),vec3(255.,221.,79.),vec3(255.,255.,255.),vec3(161.,118.,73.),vec3(255.,188.,196.))[clamp(color,1u,18u)-1u]/255.;
 return select(pow((srgb+vec3(.055))/1.055,vec3(2.4)),srgb/12.92,srgb<=vec3(.04045));
}
