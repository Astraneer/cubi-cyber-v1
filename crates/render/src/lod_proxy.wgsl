struct Camera{view_projection:mat4x4<f32>,eye:vec4<f32>,direction:vec4<f32>,params:vec4<u32>};
struct Object{model:mat4x4<f32>,inverse_model:mat4x4<f32>,bounds_min:vec4<f32>,bounds_max:vec4<f32>,data:vec4<u32>,light:vec4<f32>,reserved:array<vec4<u32>,4>};
@group(0) @binding(0)var<uniform>camera:Camera;
@group(0) @binding(1)var<storage,read>objects:array<Object>;
@group(0) @binding(2)var<storage,read>payload:array<u32>;
@group(0) @binding(3)var<storage,read>visible:array<u32>;
@group(0) @binding(4)var colors:texture_2d_array<f32>;
@group(0) @binding(5)var tint_masks:texture_2d_array<f32>;
@group(0) @binding(6)var texels:sampler;
@group(0) @binding(7)var<storage,read>materials:array<vec4<u32>>;
// INSTRUMENTATION
// LIGHTING
struct VertexOut{@builtin(position)position:vec4<f32>,@location(0)@interpolate(perspective,sample)local_point:vec3<f32>,@location(1)@interpolate(flat)slot:u32};
@vertex fn vertex(@builtin(vertex_index)v:u32,@builtin(instance_index)instance:u32)->VertexOut{
    let slot=visible[instance];let object=objects[slot];let face=v/6u;let corners=array<u32,6>(0u,1u,2u,2u,1u,3u);let corner=corners[v%6u];let origins=array<vec3<f32>,6>(vec3(0.,0.,0.),vec3(0.,16.,16.),vec3(16.,0.,0.),vec3(0.,0.,16.),vec3(0.,0.,0.),vec3(16.,0.,16.));let us=array<vec3<f32>,6>(vec3(1.,0.,0.),vec3(1.,0.,0.),vec3(-1.,0.,0.),vec3(1.,0.,0.),vec3(0.,0.,1.),vec3(0.,0.,-1.));let vs=array<vec3<f32>,6>(vec3(0.,0.,1.),vec3(0.,0.,-1.),vec3(0.,1.,0.),vec3(0.,1.,0.),vec3(0.,1.,0.),vec3(0.,1.,0.));let point=origins[face]+us[face]*f32(corner&1u)*16.+vs[face]*f32(corner>>1u)*16.;var out:VertexOut;out.position=camera.view_projection*object.model*vec4(point,1.);out.local_point=point;out.slot=slot;return out;
}
fn palette_index(object:Object,index:u32)->u32{
    let word64=index/64u;let packed=payload[object.data.x+128u+word64/2u];var rank=(packed>>((word64%2u)*16u))&65535u;let within=index%64u;if within>=32u {rank+=countOneBits(payload[object.data.x+word64*2u]);}let sub=within%32u;if sub>0u{rank+=countOneBits(payload[object.data.x+index/32u]&((1u<<sub)-1u));}let bits=object.data.z;if bits==0u{return 0u;}let bit=rank*bits;var value=payload[object.data.x+161u+bit/32u]>>(bit%32u);if bit%32u+bits>32u {value|=payload[object.data.x+161u+bit/32u+1u]<<(32u-bit%32u);}return value&((1u<<bits)-1u);
}
fn illumination(object:Object,world:vec3<f32>,normal:vec3<f32>)->vec3<f32>{// ILLUMINATION
}
struct FragmentOut{@location(0)color:vec4<f32>,@builtin(frag_depth)depth:f32};
@fragment fn fragment(input:VertexOut)->FragmentOut{
    let object=objects[input.slot];var origin=(object.inverse_model*vec4(camera.eye.xyz,1.)).xyz;var direction=normalize(input.local_point-origin);if camera.eye.w>0.5 {direction=normalize((object.inverse_model*vec4(camera.direction.xyz,0.)).xyz);origin=input.local_point-direction*64.;}
    var enter=-1e30;var exit=1e30;var entry_axis=0u;for(var axis=0u;axis<3u;axis++){if abs(direction[axis])<1e-8 {if origin[axis]<0.||origin[axis]>16. {discard;}}else{let a=(0.-origin[axis])/direction[axis];let b=(16.-origin[axis])/direction[axis];let lo=min(a,b);if lo>enter {enter=lo;entry_axis=axis;}exit=min(exit,max(a,b));}}
    if exit<max(enter,0.) {discard;}var t=max(enter,0.);let start=origin+direction*(t+0.0001);var cell=vec3<i32>(clamp(floor(start),vec3(0.),vec3(15.)));let stride=select(vec3(-1),vec3(1),direction>=vec3(0.));var delta=vec3(1e30);var next=vec3(1e30);for(var axis=0u;axis<3u;axis++){if abs(direction[axis])>=1e-8 {delta[axis]=abs(1./direction[axis]);let boundary=f32(cell[axis]+select(0,1,direction[axis]>0.));next[axis]=(boundary-origin[axis])/direction[axis];}}
    var normal=vec3(0.);normal[entry_axis]=-f32(stride[entry_axis]);
    for(var steps=1u;steps<=46u;steps++){
        let index=u32(cell.x)+16u*(u32(cell.z)+16u*u32(cell.y));let bits=payload[object.data.x+index/32u];if (bits&(1u<<(index%32u)))!=0u {
            let palette=object.data.y+palette_index(object,index)*2u;let info=payload[palette];let material=materials[info&65535u];let point=origin+direction*t;var uv=vec2(point.x,point.y);if abs(normal.x)>0.5 {uv=vec2(point.z,point.y)*vec2(-normal.x,1.);}else if abs(normal.y)>0.5{uv=vec2(point.x,point.z)*vec2(1.,-normal.y);}else{uv=vec2(point.x,point.y)*vec2(normal.z,1.);}let world=object.model*vec4(point,1.);let world_normal=normalize((transpose(object.inverse_model)*vec4(normal,0.)).xyz);let projection_pixels=f32(camera.params.y)*0.5*length(vec3(camera.view_projection[0].y,camera.view_projection[1].y,camera.view_projection[2].y));let cell_scale=length(object.model[0].xyz);let distance=max(length(world.xyz-camera.eye.xyz),0.001);let cell_pixels=projection_pixels*cell_scale/select(distance,1.,camera.eye.w>0.5);let mip=clamp(log2(f32(textureDimensions(colors).x)/max(cell_pixels,0.001)),0.,f32(textureNumLevels(colors)-1u));let texel=textureSampleLevel(colors,texels,uv,i32(material.x),mip);let kind=(info>>16u)&3u;
            if kind!=1u||texel.a>=0.5 {let mask=textureSampleLevel(tint_masks,texels,uv,i32(material.x),mip).r;let tint=unpack4x8unorm(payload[palette+1u]).rgb;let tinted=mix(vec3(1.),tint*tint,select(1.,mask,(material.y&1u)!=0u));let clip=camera.view_projection*world;var out:FragmentOut;out.color=vec4(texel.rgb*tinted*illumination(object,world.xyz,world_normal),select(1.,texel.a,kind==1u));out.depth=clip.z/clip.w;record_trace(steps,true,false);return out;}
        }
        let crossing=min(next.x,min(next.y,next.z));if crossing>exit {record_trace(steps,false,false);discard;}t=crossing;var advanced=false;for(var axis=0u;axis<3u;axis++){if next[axis]<=crossing+0.000001 {cell[axis]+=stride[axis];next[axis]+=delta[axis];if !advanced {normal=vec3(0.);normal[axis]=-f32(stride[axis]);advanced=true;}}}if any(cell<vec3(0))||any(cell>=vec3(16)){record_trace(steps,false,false);discard;}
    }
    record_trace(46u,false,true);discard;return FragmentOut(vec4(0.0),0.0);
}
