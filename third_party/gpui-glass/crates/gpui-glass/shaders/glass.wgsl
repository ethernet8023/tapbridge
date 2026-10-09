// Coordinates are physical pixels except explicitly logical optical parameters.
// Capture -> filtered reduction pyramid -> normalized separable Gaussian -> glass composite.
struct Uniforms {
    viewport: vec4<f32>, // viewport.xy, scale, inherited opacity
    bounds: vec4<f32>,   // physical origin.xy, size.xy
    crop: vec4<f32>,     // physical origin.xy, size.xy
    transmission_crop: vec4<f32>, // independently bounded refraction/frost capture
    bleed_crop: vec4<f32>, // native backdrop origin/extent, physical pixels
    bleed_damage: vec4<f32>, // conservative changed scene rectangle, physical LTRB
    optics: vec4<f32>,   // blur, tint opacity, saturation, edge width (logical pixels)
    lens: vec4<f32>,     // distortion (logical px), profile [0,2], dispersion, splay
    finish: vec4<f32>,   // light master, atlas pad (logical), fog sigma, unused
    fog_params: vec4<f32>, // independent edge width, opacity
    outline_sharp: vec4<f32>, // outline opacity/width, sharp opacity/width
    soft_inner: vec4<f32>, // soft opacity/width, inner shadow opacity/radius
    outer: vec4<f32>, // shadow opacity/radius, draw outset, unused
    reflection: vec4<f32>, // edge intensity/width, diffuse intensity/radius
    reflection_info: vec4<f32>, // sample radius, sharpness, maximum mip, unused
    reflection_modes: vec4<f32>, // edge/diffuse/shadow blend, shadow reflection intensity
    reflection_tone: vec4<f32>, // brightness floor/ceiling, dark-surface bias
    reflection_edge_tone: vec4<f32>,
    light_color: vec4<f32>, // sharp, soft, inner-shadow source brightness
    light_profiles: vec4<f32>, // sharp/soft angular spread, inner ring thickness
    reflection_color: vec4<f32>, // saturation, vibrance
    material_color: vec4<f32>, // brightness, contrast, vibrance
    shape_transform: vec4<f32>, // affine scale.xy, rectangle flag, actual shape aspect
    tint_color: vec4<f32>,
    surface: vec4<f32>, // opaque surface RGB, enabled
    sharp_position: vec4<f32>, // direction.xy, inward band center, angular focus
    soft_position: vec4<f32>,
    shadow_position: vec4<f32>, // direction.xy, inward band center, opposite lobe weight
    light_modes: vec4<f32>, // sharp/soft/shadow blend operators
    shadow_offset: vec4<f32>, // outer shadow translation.xy
    blur_info: vec4<f32>, // inverse blur target dimensions.xy, paired tap count
    kernel: array<vec4<f32>,8>, // each entry: bilinear offset, normalized weight
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var linear: sampler;
@group(0) @binding(3) var frost: texture_2d<f32>;
@group(0) @binding(5) var fog: texture_2d<f32>;
@group(0) @binding(6) var reflected: texture_2d<f32>;
@group(0) @binding(7) var diffuse_field: texture_2d<f32>;
@group(0) @binding(8) var edge_field: texture_2d<f32>;
@group(0) @binding(4) var shape: texture_2d<f32>; // signed distance, continuous outward direction.xy
struct Vertex { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> };
@vertex fn fullscreen(@builtin(vertex_index) i:u32)->Vertex {
    let uv=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));
    return Vertex(vec4<f32>(uv*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.),0.,1.),uv);
}
@vertex fn glass_vertex(@builtin(vertex_index) i:u32)->Vertex {
    let uv=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));
    let outset=u.outer.z*u.viewport.z;
    let xy=u.bounds.xy-vec2<f32>(outset)+uv*(u.bounds.zw+2.*outset);
    return Vertex(vec4<f32>(xy/u.viewport.xy*vec2<f32>(2.,-2.)+vec2<f32>(-1.,1.),0.,1.),(xy-u.bounds.xy)/u.bounds.zw);
}
@fragment fn capture(v:Vertex)->@location(0) vec4<f32> {
    return textureSampleLevel(tex,linear,(u.transmission_crop.xy+v.uv*u.transmission_crop.zw)/u.viewport.xy,0.);
}
fn field(local:vec2<f32>)->vec3<f32> {
    let logical=u.bounds.zw/u.viewport.z;
    if all(u.shape_transform.xy==vec2<f32>(1.)) {
        return textureSampleLevel(shape,linear,(local+u.finish.y)/(logical+2.*u.finish.y),0.).rgb;
    }
    let scale=max(u.shape_transform.xy,vec2<f32>(0.05));
    let q=(local-logical*0.5)/scale+logical*0.5;
    let bounded=clamp(q,vec2<f32>(-u.finish.y+0.5),logical+u.finish.y-0.5);
    let f=textureSampleLevel(shape,linear,(bounded+u.finish.y)/(logical+2.*u.finish.y),0.).rgb;
    let magnitude=length(f.yz);
    let normal=f.yz/max(magnitude,0.0001);
    let gradient=normal/scale;
    let metric=select(min(scale.x,scale.y),1./max(length(gradient),0.0001),magnitude>0.001);
    let distance=select(f.x,length(q-bounded+normal*f.x),any(q!=bounded));
    return vec3<f32>(distance*metric,gradient/max(length(gradient),0.0001)*magnitude);
}
// Antialiased 2:1 reductions. Wide blur never stretches taps across unsampled pixels.
@fragment fn reduce(v:Vertex)->@location(0) vec4<f32> {
    let o=0.5/vec2<f32>(textureDimensions(tex));
    return (textureSampleLevel(tex,linear,v.uv+vec2<f32>(o.x,o.y),0.)+
            textureSampleLevel(tex,linear,v.uv+vec2<f32>(-o.x,o.y),0.)+
            textureSampleLevel(tex,linear,v.uv+vec2<f32>(o.x,-o.y),0.)+
            textureSampleLevel(tex,linear,v.uv-o,0.))*0.25;
}
fn gaussian(uv:vec2<f32>,axis:vec2<f32>)->vec4<f32> {
    let step=axis*u.blur_info.xy;
    var c=textureSampleLevel(tex,linear,uv,0.)*u.kernel[0].y;
    for(var i=1u;i<=u32(u.blur_info.z);i+=1u) {
        let k=u.kernel[i];
        c+=(textureSampleLevel(tex,linear,uv+step*k.x,0.)+textureSampleLevel(tex,linear,uv-step*k.x,0.))*k.y;
    }
    return c;
}
@fragment fn blur_h(v:Vertex)->@location(0) vec4<f32> { return gaussian(v.uv,vec2<f32>(1.,0.)); }
@fragment fn blur_v(v:Vertex)->@location(0) vec4<f32> { return gaussian(v.uv,vec2<f32>(0.,1.)); }
fn filtered_sample(source:texture_2d<f32>,uv:vec2<f32>,dx:vec2<f32>,dy:vec2<f32>)->vec3<f32> {
    var sharp:vec3<f32>;
    let stretch=max(length(dx*vec2<f32>(textureDimensions(source))),length(dy*vec2<f32>(textureDimensions(source))));
    if stretch>1.25 {
        // Integrate the refracted pixel footprint, only where the lens minifies the image.
        // Eight anisotropic taps on the rim; the flat interior still uses one bilinear sample.
        var major=dx;var minor=dy;
        if length(dy*vec2<f32>(textureDimensions(source)))>length(dx*vec2<f32>(textureDimensions(source))) {major=dy;minor=dx;}
        sharp=vec3<f32>(0.);
        for(var i=0u;i<4u;i+=1u) {
            let o=major*((f32(i)+0.5)/4.-0.5);
            sharp+=textureSampleLevel(source,linear,uv+o+minor*0.25,0.).rgb;
            sharp+=textureSampleLevel(source,linear,uv+o-minor*0.25,0.).rgb;
        }
        sharp*=0.125;
    } else {sharp=textureSampleLevel(source,linear,uv,0.).rgb;}
    return sharp;
}
fn backdrop(reflection_uv:vec2<f32>,fog_mask:f32,reflection_dx:vec2<f32>,reflection_dy:vec2<f32>)->vec3<f32> {
    let ratio=u.crop.zw/u.transmission_crop.zw;
    let uv=(u.crop.xy+reflection_uv*u.crop.zw-u.transmission_crop.xy)/u.transmission_crop.zw;
    let dx=reflection_dx*ratio; let dy=reflection_dy*ratio;
    // Frost is a prefiltered backdrop. It is never attenuated by the edge profile.
    var color:vec3<f32>;
    if u.optics.x>=0.05 {color=filtered_sample(frost,uv,dx,dy);}
    else {color=filtered_sample(tex,uv,dx,dy);}
    // Fog is a separate, optional interior layer. At zero its texture/passes are omitted.
    if u.finish.z>=0.05 {color=mix(color,textureSampleLevel(fog,linear,uv,0.).rgb,fog_mask);}
    return color;
}
// Hardware anisotropic/trilinear filtering handles compression of a wide exterior
// strip into a thin rim. The tangent footprint grows with source distance, so
// nearby details remain crisp without shimmering along the compressed normal.
fn reflection_sample(uv:vec2<f32>)->vec4<f32> {
    let value=textureSampleLevel(edge_field,linear,uv,0.);
    return vec4<f32>(value.rgb/max(value.a,0.0001),value.a);
}
// Native-27 curved backdrop lookup; legacy mode uses the cached exterior field.
// Continuous atlas directions avoid nearest-edge projection seams.
fn diffuse_sample(uv:vec2<f32>)->vec4<f32> {
    if u.reflection_tone.w>0.5 {
        let local=(u.crop.xy+uv*u.crop.zw-u.bounds.xy)/u.viewport.z;
        let f=field(local);
        // Regular AppKit recipe: amount == height == .35 * min(size).
        // Blur is INDEPENDENT. The host halves inputBleedBlurRadius.
        let height=max(u.finish.w,0.001);
        let t=clamp(-f.x/height,0.,1.);
        let shift=height*(1.-sqrt(max(t*(2.-t),0.)));
        let sample_uv=(u.crop.xy+uv*u.crop.zw+f.yz*shift*u.viewport.z-u.bleed_crop.xy)/u.bleed_crop.zw;
        let source_step=u.bleed_crop.zw/vec2<f32>(textureDimensions(reflected));
        let radius=u.reflection.w*0.5*u.viewport.z/max(source_step.x,source_step.y);
        let footprint=select(radius,1.+radius*0.5,radius<2.);
        let lod=clamp(log2(max(footprint,1.)),0.,f32(textureNumLevels(reflected)-1u));
        let value=textureSampleLevel(reflected,linear,sample_uv,lod);
        let color=value.rgb/max(value.a,0.000001);
        return vec4<f32>(select(color,vec3<f32>(0.),abs(color)<vec3<f32>(0.000001)),value.a);
    }
    let value=textureSampleLevel(diffuse_field,linear,uv,0.);
    return vec4<f32>(value.rgb/max(value.a,0.0001),value.a);
}
// Reconstructed from macOS 27 QuartzCore AIR (26A428); see RESEARCH.md.
// Odd seventh-order approximation to a blurred step, with finite smooth support.
fn shadow_step27(x_in:f32)->f32 {
    let x=clamp(x_in,-2.,2.); let x2=x*x;
    return 0.5+x*(-0.560546875+x2*(0.168212890625+x2*(-0.034454345703125+x2*0.0029544830322265625)));
}
fn shadow_profile(distance:f32,radius:f32)->f32 {
    return clamp(shadow_step27(distance/(max(radius,0.25)*1.41421356237)),0.,1.);
}
fn key_fill27(normal:vec2<f32>,depth:f32,width:f32,position:vec4<f32>,spread:f32,aa:f32,softness:f32)->f32 {
    let z=depth-position.z;
    let ramp=1.-clamp(z/max(width,0.0001),0.,1.);
    let core=mix(select(0.,1.,ramp>0.),ramp,softness);
    let radial=core*clamp((width-z)/aa+0.5,0.,1.)*clamp(z/aa+0.5,0.,1.);
    let direction=dot(normal,position.xy);
    let lobes=clamp((vec2<f32>(direction,-direction)-spread)/max(1.-spread,0.000001),vec2<f32>(0.),vec2<f32>(1.))*radial;
    let shaped=lobes/(1.+position.w*(1.-lobes));
    return shaped.x+u.shadow_position.w*shaped.y;
}
fn key_fill_color27(color:vec3<f32>,strength:f32,highlight:f32)->vec3<f32> {
    // Native NSGlassEffectView's 27 vibrantColorMatrix. It transforms the
    // destination material under the SDF mask, so the lift adapts to its color.
    let transformed=vec3<f32>(
        dot(color,vec3<f32>(1.1201999,-0.1894,-0.019)),
        dot(color,vec3<f32>(-0.0563,0.98709995,-0.0191)),
        dot(color,vec3<f32>(-0.0563,-0.1893,1.1574)))+0.1471;
    return mix(color,clamp(transformed,vec3<f32>(0.),vec3<f32>(1.)),clamp(strength*highlight,0.,1.));
}
fn finish_blend(raw_base:vec3<f32>,raw_tint:vec3<f32>,amount:f32,mode:f32)->vec3<f32> {
    // Contrast operators are defined on display-referred [0,1] inputs.
    // Saturation can make the transmitted base negative; feeding that into the
    // soft-light cubic reverses its slope and produces dark contour rings.
    let base=clamp(raw_base,vec3<f32>(0.),vec3<f32>(1.));
    let tint=clamp(raw_tint,vec3<f32>(0.),vec3<f32>(1.));
    var mixed=1.-(1.-base)*(1.-tint); // screen
    if mode>0.5 && mode<1.5 {
        let curve=select(((16.*base-12.)*base+4.)*base,sqrt(max(base,vec3<f32>(0.))),base>vec3<f32>(0.25));
        mixed=select(base-(1.-2.*tint)*base*(1.-base),base+(2.*tint-1.)*(curve-base),tint>vec3<f32>(0.5));
    } else if mode>1.5 && mode<2.5 { mixed=base+tint; }
    else if mode>2.5 && mode<3.5 { mixed=base*tint; }
    else if mode>3.5 && mode<4.5 { mixed=base+tint-1.; }
    else if mode>4.5 && mode<5.5 { mixed=tint; }
    else if mode>5.5 { mixed=select(2.*base*tint,1.-2.*(1.-base)*(1.-tint),base>vec3<f32>(0.5)); }
    return mix(base,clamp(mixed,vec3<f32>(0.),vec3<f32>(1.)),clamp(amount,0.,1.));
}
fn directional_lobes(normal:vec2<f32>,direction:vec2<f32>,focus:f32)->f32 {
    let facing=dot(normal,direction);
    return pow(max(facing,0.),max(focus,0.1))+u.shadow_position.w*pow(max(-facing,0.),max(focus,0.1));
}
// Gate radiance BEFORE tone mapping. Equal surroundings are not a reflection
// signal, and no-support pixels stay exactly unchanged for every blend operator.
fn adjusted_chroma(raw:vec3<f32>,saturation:f32,vibrance:f32)->vec3<f32> {
    let c=clamp(raw,vec3<f32>(0.),vec3<f32>(1.));
    let l=dot(c,vec3<f32>(0.2126,0.7152,0.0722));
    let span=max(max(c.r,c.g),c.b)-min(min(c.r,c.g),c.b);
    let source_saturation=span/max(max(max(c.r,c.g),c.b),0.00001);
    let muted=1.-source_saturation;
    let strength=saturation*(1.+vibrance*muted*muted);
    let chroma=(c-vec3<f32>(l))*strength;
    // Compress along the chroma axis into gamut, preserving hue and luminance.
    let positive=max(max(chroma.r,chroma.g),max(chroma.b,0.00001));
    let negative=min(min(chroma.r,chroma.g),min(chroma.b,-0.00001));
    let scale=min(1.,min((1.-l)/positive,-l/negative));
    return vec3<f32>(l)+chroma*scale;
}
fn reflection_color(raw:vec3<f32>)->vec3<f32> {
    let graded=(raw-vec3<f32>(0.5))*u.reflection_color.w+vec3<f32>(0.5+u.reflection_color.z);
    return adjusted_chroma(graded,u.reflection_color.x,u.reflection_color.y);
}
fn reflected_blend(raw_base:vec3<f32>,radiance:vec4<f32>,amount:f32,mode:f32,tone:vec2<f32>)->vec3<f32> {
    let base=clamp(raw_base,vec3<f32>(0.),vec3<f32>(1.));
    let contrast=max(max(abs(radiance.r-base.r),abs(radiance.g-base.g)),abs(radiance.b-base.b));
    // A continuous neutral rejection avoids onset popping from low-contrast support.
    let visible_contrast=max(contrast-0.001,0.);
    // Cap weak signals instead of multiplying two attenuation terms. This
    // retains linear near-source energy while neutral support fades in smoothly.
    let signal=min(radiance.a,visible_contrast*4.);
    let luminance=dot(base,vec3<f32>(0.2126,0.7152,0.0722));
    let tint=mix(vec3<f32>(tone.x),vec3<f32>(tone.y),reflection_color(radiance.rgb));
    if mode>6.5 {
        // Bright radiance screens onto dark surfaces. Near white, reserve a small
        // absorption budget for hue: purely additive light cannot color white.
        // Gray/black sources cause no broad darkening; high intensities approach
        // a bounded response smoothly rather than clipping at an arbitrary value.
        let t=clamp(tint,vec3<f32>(0.),vec3<f32>(1.));
        let peak=max(max(t.r,t.g),t.b);
        // QuartzCore 27 uses complementary fourth-power luminance gates for
        // bleed. Retain an adjustable floor so colored midtones remain visible.
        let light2=luminance*luminance;
        let dark2=(1.-luminance)*(1.-luminance);
        let glow_gate=mix(1.,dark2*dark2,u.reflection_tone.z);
        let tint_gate=mix(1.,light2*light2,u.reflection_tone.z);
        var absorption=base*(vec3<f32>(peak)-t)*tint_gate*0.3;
        absorption*=min(1.,0.08/max(dot(absorption,vec3<f32>(0.2126,0.7152,0.0722)),0.0001));
        let energy=max(amount*signal,0.)*3.;
        let response=energy/(1.+energy);
        return clamp(base+((1.-base)*t*glow_gate-absorption)*response,vec3<f32>(0.),vec3<f32>(1.));
    }
    let dark=mix(1.,1.-clamp(luminance,0.,1.),u.reflection_tone.z);
    return finish_blend(base,tint,amount*signal*dark,mode);
}
fn highlight_blend(color:vec3<f32>,amount:f32,mode:f32,brightness:f32)->vec3<f32> {
    return finish_blend(color,vec3<f32>(brightness),amount,mode);
}
@fragment fn glass(v:Vertex)->@location(0) vec4<f32> {
    let logical=u.bounds.zw/u.viewport.z;
    let local=v.uv*logical;
    let atlas=field(local);
    let d=atlas.r;
    let sdf_width=max(fwidth(d),0.0001);
    let aa=0.75/u.viewport.z;
    let coverage=1.-smoothstep(-aa,aa,d);
    let depth=max(-d,0.);
    // Interior directions are harmonically extended from the contour in the
    // cached atlas. Keep their magnitude: opposing boundaries cancel smoothly.
    let normal=atlas.gb;
    let boundary=local-normal*d;
    let base=(u.bounds.xy+v.uv*u.bounds.zw-u.crop.xy)/u.crop.zw;
    var profile=0.;
    var direction=normal;
    var delta=vec2<f32>(0.);
    var dx=vec2<f32>(0.);
    var dy=vec2<f32>(0.);
    // Opaque reflection-only panels skip lens math as well as backdrop passes.
    // This branch is uniform, before the coverage discard, so derivatives remain valid.
    if u.surface.w<0.5 {
        let edge=max(u.optics.w,0.5);
        if d> -edge && (u.lens.x!=0. || u.lens.z>0.) {
            let t=clamp(1.+d/edge,0.,1.);
            let circular=1.-sqrt(max(1.-t*t,0.));
            profile=mix(t,circular,clamp(u.lens.y,0.,1.));
            if u.lens.y>1. {
                let t2=t*t;
                let superellipse=1.-sqrt(sqrt(max(1.-t2*t2,0.)));
                profile=mix(profile,superellipse,clamp(u.lens.y-1.,0.,1.));
            }
            if u.lens.w>0. {
                // Native 27 gradientOvalization: center-relative, aspect-corrected
                // rays, NOT rays from the projected boundary. Normalize before
                // mixing, then retain our continuous atlas cancellation at medial
                // axes for oversized bands (native analytic normals are unit length).
                let centered=(local-logical*0.5)*vec2<f32>(1.,u.shape_transform.w);
                let radial=centered/max(length(centered),0.00001);
                let magnitude=length(normal);
                let unit_normal=normal/max(magnitude,0.00001);
                let fanned=mix(unit_normal,radial,clamp(u.lens.w,0.,1.));
                direction=fanned/max(length(fanned),0.00001)*min(magnitude,1.);
            }
            delta=direction*u.lens.x*profile*u.viewport.z/u.crop.zw;
        }
        dx=dpdx(base-delta);
        dy=dpdy(base-delta);
    }
    var shadow=0.;
    var shadow_rgb=vec3<f32>(0.);
    var shadow_tint_alpha=0.;
    if (u.outer.x>0. || u.reflection_modes.w>0.) && coverage<1. {
        let contact_d=field(local-u.shadow_offset.xy*0.25).x;
        let penumbra_d=field(local-u.shadow_offset.xy).x;
        let strength=clamp(u.outer.x,0.,1.);
        let contact=shadow_profile(contact_d,u.outer.y*0.28)*strength*0.55;
        let penumbra=shadow_profile(penumbra_d,u.outer.y)*strength*0.75;
        shadow=1.-(1.-contact)*(1.-penumbra);
        if u.reflection_modes.w>0. {
            let radiance=diffuse_sample(base);
            // Tint the existing shadow with exterior chroma, not source luminance.
            // Achromatic surroundings leave a black shadow unchanged. Keep the
            // original alpha/profile and bound the tint below highlight levels.
            let source_color=reflection_color(radiance.rgb);
            let low=min(min(source_color.r,source_color.g),source_color.b);
            let high=max(max(source_color.r,source_color.g),source_color.b);
            let chroma=source_color-vec3<f32>(low);
            let hue=chroma/max(high-low,0.0001);
            let saturation=(high-low)/(high-low+0.08);
            let blended=finish_blend(vec3<f32>(0.18),hue*0.65,0.75,u.reflection_modes.z);
            let neutral=min(min(blended.r,blended.g),blended.b);
            let tint=(blended-vec3<f32>(neutral))*0.8;
            let profile_contact=shadow_profile(contact_d,u.outer.y*0.28)*0.55;
            let profile_soft=shadow_profile(penumbra_d,u.outer.y)*0.75;
            let profile=1.-(1.-profile_contact)*(1.-profile_soft);
            shadow_tint_alpha=profile*clamp(u.reflection_modes.w*radiance.a*saturation,0.,1.);
            shadow_rgb=tint*shadow_tint_alpha;
        }
    }
    var outline=0.;
    var outline_rgb=vec3<f32>(0.);
    if u.outline_sharp.x>0. && coverage<1. {
        let expanded=1.-smoothstep(u.outline_sharp.y-aa,u.outline_sharp.y+aa,d);
        // Normalize by the exterior coverage so the dark stroke is outside the
        // material, over the shadow, without intruding into its bright inner rim.
        outline=clamp(u.outline_sharp.x,0.,1.)*clamp((expanded-coverage)/max(1.-coverage,0.00001),0.,1.);
        if u.light_modes.x>7.5 {
            // Background key/fill is the OUTER dark rim in 27: native bias -.1875,
            // height1, offset-1, spreadcos(120deg), CAamount.5 -> rationalamount0.
            let position=vec4<f32>(0.,-1.,-u.outline_sharp.y,0.);
            let h=key_fill27(normal,-d,u.outline_sharp.y,position,-0.5,sdf_width,0.75);
            var backdrop_color=u.surface.rgb;
            if u.surface.w<0.5 {
                let uv=(u.bounds.xy+v.uv*u.bounds.zw-u.transmission_crop.xy)/u.transmission_crop.zw;
                backdrop_color=textureSampleLevel(tex,linear,uv,0.).rgb;
            }
            let shadow_a=shadow_tint_alpha+(1.-shadow_tint_alpha)*shadow;
            let under=clamp(shadow_rgb+(1.-shadow_a)*backdrop_color,vec3<f32>(0.),vec3<f32>(1.));
            let attenuation=clamp(u.outline_sharp.x*h*(3.-2.*under),vec3<f32>(0.),vec3<f32>(1.));
            outline=max(max(attenuation.r,attenuation.g),attenuation.b);
            outline_rgb=under*(outline-attenuation);
        }
    }
    let shadow_alpha=shadow_tint_alpha+(1.-shadow_tint_alpha)*shadow;
    let exterior_alpha=outline+(1.-outline)*shadow_alpha;
    let exterior_rgb=outline_rgb+shadow_rgb*(1.-outline);
    if coverage<=0.001 {
        if exterior_alpha<=0.0001 { discard; }
        return vec4<f32>(exterior_rgb*u.viewport.w,exterior_alpha*u.viewport.w);
    }
    var color=u.surface.rgb;
    if u.surface.w<0.5 {
        var fog_mask=0.;
        if u.finish.z>=0.05 {
            fog_mask=u.fog_params.y;
            if u.fog_params.x>0. { fog_mask=mix(u.fog_params.z,u.fog_params.y,smoothstep(0.,u.fog_params.x,depth)); }
        }
        if u.lens.z>0.001 && profile>0.001 {
            let split=direction*u.lens.z*(2.+abs(u.lens.x)*0.12)*profile*u.viewport.z/u.crop.zw;
            let center=base-delta;
            color=backdrop(center-split,fog_mask,dx,dy)*vec3<f32>(0.22,0.04,0.01)
                 +backdrop(center-split*0.5,fog_mask,dx,dy)*vec3<f32>(0.40,0.22,0.10)
                 +backdrop(center,fog_mask,dx,dy)*vec3<f32>(0.27,0.48,0.27)
                 +backdrop(center+split*0.5,fog_mask,dx,dy)*vec3<f32>(0.10,0.22,0.40)
                 +backdrop(center+split,fog_mask,dx,dy)*vec3<f32>(0.01,0.04,0.22);
        } else { color=backdrop(base-delta,fog_mask,dx,dy); }
        color=(color-vec3<f32>(0.5))*u.material_color.y+vec3<f32>(0.5+u.material_color.x);
        color=adjusted_chroma(color,u.optics.z,u.material_color.z);
        color=mix(color,u.tint_color.rgb,u.optics.y);
    }
    if u.reflection.z>0. && u.reflection.w>0. && (u.reflection_tone.w>0.5 || depth<u.reflection.w) {
        let reflected_color=diffuse_sample(base);
        // Native recipe distances are 1 and 0, not height and zero.
        let band=select(clamp(1.-depth/max(u.reflection.w,0.01),0.,1.),clamp(1.-d,0.,1.),u.reflection_tone.w>0.5);
        let weight=u.reflection.z*band*band;
        color=reflected_blend(color,reflected_color,weight,u.reflection_modes.y,u.reflection_tone.xy);
    }
    if u.reflection.x>0. && u.reflection.y>0. && depth<u.reflection.y {
        let reflected_color=reflection_sample(v.uv);
        let weight=u.reflection.x*(1.-depth/u.reflection.y)*sqrt(1.-depth/u.reflection.y);
        color=reflected_blend(color,reflected_color,weight,u.reflection_modes.x,u.reflection_edge_tone.xy);
    }
    // Native 27 path: distance-band rational key/fill, after the inner ring.
    // Legacy blend operators remain selectable for experiments and old presets.
    if u.soft_inner.z>0. {
        if u.light_modes.z>7.5 && depth<abs(u.shadow_position.z)+u.light_profiles.z+u.soft_inner.w*2.829 {
            let shifted=field(local+u.shadow_position.xy*u.shadow_position.z).x;
            let radius=max(u.soft_inner.w,0.001)*1.41421356237;
            let ring=clamp(shadow_step27(shifted/radius)-shadow_step27((shifted+u.light_profiles.z)/radius),0.,1.);
            color=mix(color,vec3<f32>(u.light_color.z),clamp(u.soft_inner.z*ring,0.,1.));
        } else if u.light_modes.z<=7.5 {
            let facing=max(dot(normal,u.shadow_position.xy),0.);
            let directional=facing*sqrt(facing);
            let clear_rim=1.-exp(-depth*1.5);
            let z=(depth-u.shadow_position.z)/u.soft_inner.w;
            let band=clear_rim*exp(-0.5*z*z);
            color=finish_blend(color,vec3<f32>(u.light_color.z),u.soft_inner.z*directional*band,u.light_modes.z);
        }
    }
    var native_highlight=0.;
    if u.soft_inner.x>0. {
        if u.light_modes.y>7.5 && depth>=u.soft_position.z-sdf_width && depth<=u.soft_position.z+u.soft_inner.y+sdf_width {
            let highlight=key_fill27(normal,depth,u.soft_inner.y,u.soft_position,u.light_profiles.y,sdf_width,1.);
            native_highlight+=u.soft_inner.x*u.light_color.y*highlight;
        } else if u.light_modes.y<=7.5 {
            let directional=directional_lobes(normal,u.soft_position.xy,u.soft_position.w);
            let z=(depth-u.soft_position.z)/u.soft_inner.y;
            let band=exp(-0.5*z*z);
            color=highlight_blend(color,u.soft_inner.x*directional*band,u.light_modes.y,u.light_color.y);
        }
    }
    if u.outline_sharp.z>0. {
        if u.light_modes.x>7.5 && depth>=u.sharp_position.z-sdf_width && depth<=u.sharp_position.z+u.outline_sharp.w+sdf_width {
            let highlight=key_fill27(normal,depth,u.outline_sharp.w,u.sharp_position,u.light_profiles.x,sdf_width,0.75);
            native_highlight+=u.outline_sharp.z*u.light_color.x*highlight;
        } else if u.light_modes.x<=7.5 {
            let directional=directional_lobes(normal,u.sharp_position.xy,u.sharp_position.w);
            let z=(depth-u.sharp_position.z)/u.outline_sharp.w;
            let band=exp(-0.5*z*z);
            color=highlight_blend(color,u.outline_sharp.z*directional*band,u.light_modes.x,u.light_color.x);
        }
    }
    // Native sharp and diffuse key/fill share one mask and one destination
    // transform. Applying the matrix twice lifts and clips the overlap twice.
    if native_highlight>0. { color=key_fill_color27(color,1.,native_highlight); }
    let alpha=(coverage+(1.-coverage)*exterior_alpha)*u.viewport.w;
    let rgb=clamp(color,vec3<f32>(0.),vec3<f32>(1.))*coverage+(1.-coverage)*exterior_rgb;
    return vec4<f32>(rgb*u.viewport.w,alpha);
}
