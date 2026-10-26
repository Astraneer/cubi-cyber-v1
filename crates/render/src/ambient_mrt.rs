//! Loading-selected signed removable ambient contribution for contact AO.
//! The first output retains the original expression and its rounding.
const OUTPUT: &str =
    "struct AmbientOutput{@location(0)color:vec4<f32>,@location(1)ambient:vec4<f32>};\n";
pub(crate) fn targets(
    blend: Option<wgpu::BlendState>,
    enabled: bool,
) -> Vec<Option<wgpu::ColorTargetState>> {
    vec![
        Some(wgpu::ColorTargetState {
            format: wgpu::TextureFormat::Rgba16Float,
            blend,
            write_mask: wgpu::ColorWrites::ALL
        });
        if enabled { 2 } else { 1 }
    ]
}
fn changed(source: &str, before: &str, after: &str) -> String {
    assert!(
        source.contains(before),
        "ambient MRT source contract missing: {before}"
    );
    source.replace(before, after)
}
fn indirect(lit: bool, atmosphere: bool, gi: bool) -> String {
    let base = if atmosphere {
        "light.rgb+light.a*atmosphere_diffuse(normal)"
    } else if lit {
        "light.rgb+vec3(light.a*0.15)"
    } else {
        "vec3(0.65)"
    };
    format!(
        "{base}{}",
        if gi {
            "+model_reflected_irradiance(world,normal)*0.3183098862"
        } else {
            ""
        }
    )
}
pub(crate) fn entity(source: &str, shadows: bool, atmosphere: bool, gi: bool) -> String {
    let source = changed(
        source,
        "@fragment fn fragment(input:VertexOut)->@location(0)vec4<f32>",
        "@fragment fn fragment(input:VertexOut)->AmbientOutput",
    );
    let source = changed(
        &source,
        "color*=illumination(input.world,normalize(input.normal),input.light);",
        "var ambient=color*ambient_illumination(input.world,normalize(input.normal),input.light);color*=illumination(input.world,normalize(input.normal),input.light);",
    );
    let source = changed(
        &source,
        "color=mix(color,vec3(1.,0.08,0.05),0.6);",
        "color=mix(color,vec3(1.,0.08,0.05),0.6);ambient*=0.4;",
    );
    let source = changed(
        &source,
        "return vec4(color,texel.a);",
        "return AmbientOutput(vec4(color,texel.a),vec4(ambient,texel.a));",
    );
    format!(
        "{OUTPUT}{source}\nfn ambient_illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{{return {};}}",
        indirect(shadows, atmosphere, gi)
    )
}
pub(crate) fn block(source: &str, shadows: bool, atmosphere: bool, gi: bool) -> String {
    let source = changed(source, "->@location(0)vec4<f32>", "->AmbientOutput");
    let source = changed(
        &source,
        "let radiance=illumination(input.world,n,input.light);",
        "let radiance=illumination(input.world,n,input.light);let ambient=ambient_illumination(input.world,n,input.light);let direct=radiance-ambient;",
    );
    let source = changed(
        &source,
        "return vec4(machine_port_linear(input.port_mode)*radiance,.6);",
        "return AmbientOutput(vec4(machine_port_linear(input.port_mode)*radiance,.6),vec4(machine_port_linear(input.port_mode)*ambient,.6));",
    );
    let source = changed(
        &source,
        "return vec4(color,texel.a);",
        "let delta=base*(max(radiance,vec3(input.glow))-max(direct,vec3(input.glow)))+(albedo-base)*ambient;return AmbientOutput(vec4(color,texel.a),vec4(delta,texel.a));",
    );
    let source = changed(
        &source,
        "return vec4(glass.rgb*connector_linear(input.connector_color)*light,glass.a);",
        "return AmbientOutput(vec4(glass.rgb*connector_linear(input.connector_color)*light,glass.a),vec4(glass.rgb*connector_linear(input.connector_color)*ambient_illumination(input.world,normal,input.light),glass.a));",
    );
    format!(
        "{OUTPUT}{source}\nfn ambient_illumination(world:vec3<f32>,normal:vec3<f32>,light:vec4<f32>)->vec3<f32>{{return {};}}",
        indirect(shadows, atmosphere, gi)
    )
}
pub(crate) fn terrain(source: &str, lit: bool, atmosphere: bool, gi: bool, pack: bool) -> String {
    let mut source = changed(
        source,
        "fn masked_fragment(input: VertexOut) -> @location(0) vec4<f32>",
        "fn masked_fragment(input: VertexOut) -> AmbientOutput",
    );
    source = changed(
        &source,
        "fn fragment(input:OrdinaryFragmentIn)->@location(0)vec4<f32>",
        "fn fragment(input:OrdinaryFragmentIn)->AmbientOutput",
    );
    // Both the donor-atlas path and prepared local material fallback use this.
    source = changed(
        &source,
        "world_position:vec3<f32>)->vec4<f32>",
        "world_position:vec3<f32>)->AmbientOutput",
    );
    source = changed(
        &source,
        "return vec4(material_radiance(surface_light(texel.rgb*tinted,ao,normal,slot,local_position,local_normal,world_position),texel.rgb*tinted,material),coverage);",
        "return AmbientOutput(vec4(material_radiance(surface_light(texel.rgb*tinted,ao,normal,slot,local_position,local_normal,world_position),texel.rgb*tinted,material),coverage),vec4(select(surface_ambient(texel.rgb*tinted,ao,normal,slot,local_position,local_normal,world_position),vec3(0.0),bucket%3u==2u),coverage));",
    );
    let input = if lit {
        "let light=sample_page_light(slot,local_position,local_normal);"
    } else {
        "let light=vec4(0.0,0.0,0.0,1.0);"
    };
    let base = if atmosphere {
        if lit {
            "light.rgb+light.a*atmosphere_diffuse(normal)"
        } else {
            "atmosphere_diffuse(normal)"
        }
    } else if lit {
        "light.rgb+vec3(light.a*0.15)"
    } else {
        "vec3(0.65)"
    };
    let gi = if gi {
        "+reflected_irradiance(world_position,normal)*0.3183098862"
    } else {
        ""
    };
    let ambient = format!("{input}return albedo*({base}{gi})*ao;");
    if pack {
        source = changed(
            &source,
            "tangent_v:vec3<f32>)->vec4<f32>",
            "tangent_v:vec3<f32>)->AmbientOutput",
        );
        source = changed(
            &source,
            "return vec4(pack_light(texel.rgb*tinted,pbr,ao,mapped_normal,view,slot,local_position,local_normal,world_position),coverage);",
            "return AmbientOutput(vec4(pack_light(texel.rgb*tinted,pbr,ao,mapped_normal,view,slot,local_position,local_normal,world_position),coverage),vec4(select(surface_ambient(texel.rgb*tinted*(vec3(1.0)-material_f0(texel.rgb*tinted,pbr))*select(1.0,0.0,pbr.metal_id>=230u),ao,mapped_normal,slot,local_position,local_normal,world_position),vec3(0.0),bucket%3u==2u),coverage));",
        );
    }
    format!(
        "{OUTPUT}{source}\nfn surface_ambient(albedo:vec3<f32>,ao:f32,normal:vec3<f32>,slot:u32,local_position:vec3<f32>,local_normal:vec3<f32>,world_position:vec3<f32>)->vec3<f32>{{{ambient}}}"
    )
}
