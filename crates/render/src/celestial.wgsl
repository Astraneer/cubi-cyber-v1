// Original procedural sun, lunar surface/phases and immutable star catalog.
struct Parameters{camera:mat4x4<f32>,sun:vec4<f32>,moon:vec4<f32>,clock:vec4<f32>,appearance:vec4<f32>,styles:vec4<u32>};
struct Star{direction:vec4<f32>,light:vec4<f32>};
struct AtmosphereLighting{r:vec4<f32>,g:vec4<f32>,b:vec4<f32>,sun:vec4<f32>,direction:vec4<f32>};
@group(0)@binding(0)var<uniform>params:Parameters;
@group(0)@binding(1)var<storage,read>stars:array<Star>;
@group(0)@binding(2)var transmittance:texture_2d<f32>;
@group(0)@binding(3)var<storage,read>atmosphere:AtmosphereLighting;
struct Out{@builtin(position)position:vec4<f32>,@location(0)uv:vec2<f32>,@location(1)@interpolate(flat)kind:u32,@location(2)@interpolate(flat)light:vec4<f32>,@location(3)@interpolate(flat)height:f32};
@vertex fn vertex(@builtin(vertex_index)vertex:u32,@builtin(instance_index)instance:u32)->Out{
 let count=params.styles.w;var direction=params.sun.xyz;var radius=params.appearance.x*3.;var kind=0u;var light=vec4(1.);
 if instance<count{
  let star=stars[instance];let p=star.direction.xyz;direction=vec3(p.x*params.clock.x+p.z*params.clock.y,p.y,-p.x*params.clock.y+p.z*params.clock.x);radius=star.direction.w;kind=2u;light=star.light;
 }else if instance==count+1u{direction=params.moon.xyz;radius=params.appearance.y;kind=1u;}
 let up=select(vec3(0.,1.,0.),vec3(1.,0.,0.),abs(direction.y)>0.95);let right=normalize(cross(direction,up));let vertical=cross(right,direction);let uv=vec2(f32(vertex&1u),f32(vertex>>1u))*2.-vec2(1.);let clip=params.camera*vec4(direction+(right*uv.x+vertical*uv.y)*radius,0.);
 var out:Out;out.position=vec4(clip.xy,0.,clip.w);if direction.y<=0.||clip.w<=0.{out.position=vec4(2.,2.,0.,1.);}out.uv=uv;out.kind=kind;out.light=light;out.height=direction.y;return out;
}
fn optical(mu:f32)->vec3<f32>{let size=vec2<i32>(textureDimensions(transmittance));let uv=vec2(mu*0.5+0.5,params.clock.w/100.);let point=vec2<i32>(uv*vec2<f32>(size));return textureLoad(transmittance,clamp(point,vec2(0),size-vec2(1)),0).rgb;}
fn crater(uv:vec2<f32>,center:vec2<f32>,radius:f32)->f32{let d=length((uv-center)/radius);return 1.-0.28*exp(-d*d*4.)+0.18*exp(-pow((d-0.82)*9.,2.));}
@fragment fn fragment(input:Out)->@location(0)vec4<f32>{
 let radius=length(input.uv);let edge=max(fwidth(radius),0.001);
 if input.kind==0u{
  var uv=input.uv*3.;if params.styles.x==2u{uv=(floor(uv*16.)+vec2(0.5))/16.;}let r=length(uv);let coverage=1.-smoothstep(1.-edge*3.,1.+edge*3.,r);var halo=exp(-r*r*2.5)*0.035;
  if params.styles.x==1u{let a=atan2(uv.y,uv.x);halo+=pow(max(cos(a*12.),0.),10.)*exp(-r*2.)*0.08;}
  let radiance=min(atmosphere.sun.rgb/max(3.141592654*params.appearance.x*params.appearance.x,0.00001),vec3(60000.));return vec4(radiance*(coverage+halo)*params.sun.w,0.);
 }
 if input.kind==1u{
  var uv=input.uv;if params.styles.y==1u{uv=(floor(uv*16.)+vec2(0.5))/16.;}let r=length(uv);if r>1.+edge{discard;}let normal=vec3(uv,sqrt(max(0.,1.-dot(uv,uv))));let phase=params.appearance.z;let incoming=vec3(sin(phase),0.,cos(phase));let illumination=max(dot(normal,incoming),0.);let seas=0.78+0.16*sin(uv.x*5.+sin(uv.y*7.))*sin(uv.y*4.);
  let surface=seas*crater(uv,vec2(-0.35,0.1),0.21)*crater(uv,vec2(0.25,-0.4),0.15)*crater(uv,vec2(0.45,0.3),0.12);let coverage=1.-smoothstep(1.-edge,1.+edge,r);
  let radiance=vec3(0.82,0.88,1.)*surface*(illumination+0.008)*params.moon.w*optical(input.height);return vec4(radiance*coverage,coverage);
 }
 if radius>1.{discard;}let night=1.-smoothstep(-0.25,-0.08,params.sun.y);let twinkle=1.+0.08*sin(params.clock.z*input.light.w+input.light.y);var shape=exp(-radius*radius*7.);if params.styles.z==1u{shape=select(0.,1.,max(abs(input.uv.x),abs(input.uv.y))<0.55);}else if params.styles.z==2u{shape+=exp(-abs(input.uv.x)*25.-abs(input.uv.y)*3.)*0.25;shape+=exp(-abs(input.uv.y)*25.-abs(input.uv.x)*3.)*0.25;}
 let color=mix(vec3(1.,0.75,0.5),vec3(0.65,0.8,1.),input.light.z);return vec4(color*input.light.x*twinkle*shape*night*params.appearance.w*optical(input.height),0.);
}
