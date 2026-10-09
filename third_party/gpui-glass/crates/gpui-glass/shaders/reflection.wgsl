// Exterior radiance capture, Gaussian pyramid and reusable reflection fields.
// The uniform layout must match glass.wgsl.
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
    reflection_info: vec4<f32>, // sample radius, sharpness, maximum mip, physical capture step
    reflection_modes: vec4<f32>, // edge/diffuse/shadow blend, shadow reflection intensity
    reflection_tone: vec4<f32>, // brightness floor/ceiling, dark-surface bias
    reflection_edge_tone: vec4<f32>,
    light_color: vec4<f32>, // sharp, soft, inner-shadow source brightness
    light_profiles: vec4<f32>, // sharp/soft angular spread, inner ring thickness
    reflection_color: vec4<f32>, // saturation, vibrance
    material_color: vec4<f32>, // brightness, contrast, vibrance
    shape_transform: vec4<f32>, // affine scale around fixed bounds center
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
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var linear: sampler;
@group(0) @binding(3) var shape: texture_2d<f32>;
@group(0) @binding(4) var mip0: texture_storage_2d<rgba16float,write>;
@group(0) @binding(5) var mip1: texture_storage_2d<rgba16float,write>;
@group(0) @binding(6) var mip2: texture_storage_2d<rgba16float,write>;
@group(0) @binding(8) var radiance_pyramid: texture_2d<f32>;
@group(0) @binding(7) var mip3: texture_storage_2d<rgba16float,write>;

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

// Analytic rectangular captures avoid atlas traffic for dense opaque panels.
// Curves/custom contours retain the shared SDF and strict exterior mask.
fn exterior_distance(local:vec2<f32>)->f32 {
    let logical=u.bounds.zw/u.viewport.z;
    if u.shape_transform.z>0.5 {
        let q=abs(local-logical*0.5)-logical*0.5;
        return length(max(q,vec2<f32>(0.)))+min(max(q.x,q.y),0.);
    }
    let clamped=clamp(local,vec2<f32>(-u.finish.y+0.5),logical+u.finish.y-0.5);
    let f=field(clamped);
    if f.x<=0. { return f.x; }
    let normal=normalize(f.yz+vec2<f32>(0.000001));
    return length(local-(clamped-normal*f.x));
}
fn exterior_radiance(screen:vec2<f32>,distance:f32)->vec4<f32> {
    if any(screen<vec2<f32>(0.)) || any(screen>=u.viewport.xy) || distance<=0.75/u.viewport.z { return vec4<f32>(0.); }
    let t=clamp(distance/max(u.reflection_info.x,0.001),0.,1.);
    let weight=1.-t*t*t*(t*(t*6.-15.)+10.);
    if weight<=0. { return vec4<f32>(0.); }
    return textureSampleLevel(source,linear,screen/u.viewport.xy,0.)*weight;
}
fn exterior_pixel(screen:vec2<f32>)->vec4<f32> {
    return exterior_radiance(screen,exterior_distance((screen-u.bounds.xy)/u.viewport.z));
}

// Dispatch ceil(mip0.width/8) by ceil(mip0.height/8) workgroups.
@compute @workgroup_size(8,8,1)
fn capture(@builtin(global_invocation_id) global:vec3<u32>) {
    let dimensions=textureDimensions(mip0);
    let pixel=min(global.xy,dimensions-vec2<u32>(1u));
    let uv=(vec2<f32>(pixel)+0.5)/vec2<f32>(dimensions);
    let screen=u.crop.xy+uv*u.crop.zw;
    let p=(screen-u.bounds.xy)/u.viewport.z;
    let distance=exterior_distance(p);
    var value=vec4<f32>(0.);
    let step=u.reflection_info.w;
    // 4x4 bilinear boxes cover a full eight-physical-pixel cell on Retina;
    // no sparse stretched taps. Only boundary cells repeat the contour test.
    if distance> -step*0.71/u.viewport.z {
        for(var y=0;y<4;y++) { for(var x=0;x<4;x++) {
            let o=(vec2<f32>(f32(x),f32(y))+0.5)/4.-0.5;
            if distance>step*0.71/u.viewport.z {
                value+=exterior_radiance(screen+o*step,distance);
            } else { value+=exterior_pixel(screen+o*step); }
        }}
        value*=0.0625;
    }
    if all(global.xy<dimensions) { textureStore(mip0,vec2<i32>(global.xy),value); }
}

// macOS 27 edge_bleed sampling, reconstructed from QuartzCore AIR.
// Source preparation remains our exterior-only Gaussian pyramid; see RESEARCH.md.
fn diffuse_sample(uv:vec2<f32>,local:vec2<f32>)->vec4<f32> {
    let f=field(local);
    let height=max(u.reflection.w,0.001);
    let t=clamp(max(-f.x,0.)/height,0.,1.);
    let amount=min(height,0.35*min(u.bounds.z/u.viewport.z,u.bounds.w/u.viewport.z));
    let shift=amount*(1.-sqrt(max(t*(2.-t),0.)));
    // Preserve the continuous direction's magnitude. Normalizing it recreates
    // opposing-normal discontinuities through the center of wide bands.
    let displaced=uv+f.yz*shift*u.viewport.z/u.crop.zw;
    // Native amount and blur are independent of height. Our radius control maps
    // to inputBleedBlurRadius=4*height (host halves it), with the observed
    // 0.35*minor-dimension amount cap. This calibration is not a native recipe.
    // Convert to coarse source texels before the native radius-to-LOD mapping.
    let radius=height*2.*u.viewport.z/u.reflection_info.w;
    let footprint=select(radius,1.+radius*0.5,radius<2.);
    let lod=clamp(log2(max(footprint,1.)),0.,u.reflection_info.z);
    let radiance=textureSampleLevel(source,linear,displaced,lod);
    // Exterior support is an independent adaptation, not native Apple math.
    // Smooth compression avoids a contour where the old gain suddenly clamps.
    let support=1.-exp(-radiance.a*max(1.,height/48.));
    return vec4<f32>(radiance.rgb/max(radiance.a,0.0001),support);
}

// Cache one native-profile mip lookup per low-resolution field pixel.
@compute @workgroup_size(8,8,1)
fn diffuse(@builtin(global_invocation_id) pixel:vec3<u32>) {
    let dimensions=textureDimensions(mip0);
    if any(pixel.xy>=dimensions) { return; }
    let uv=(vec2<f32>(pixel.xy)+0.5)/vec2<f32>(dimensions);
    let local=(u.crop.xy+uv*u.crop.zw-u.bounds.xy)/u.viewport.z;
    let logical=u.bounds.zw/u.viewport.z;
    // Only shade pixels that can be sampled by this material or its shadow.
    // Retain one bilinear-footprint guard around the entire visible rectangle.
    let guard=u.outer.z+5.;
    if any(local<vec2<f32>(-guard)) || any(local>logical+guard) {
        textureStore(mip0,vec2<i32>(pixel.xy),vec4<f32>(0.)); return;
    }
    let radiance=diffuse_sample(uv,local);
    textureStore(mip0,vec2<i32>(pixel.xy),vec4<f32>(radiance.rgb*radiance.a,radiance.a));
}

// Detailed near-rim samples come directly from the scene, while distant content
// uses the cheap coarse pyramid. Texture size follows the shape, never the halo.
@compute @workgroup_size(8,8,1)
fn edge(@builtin(global_invocation_id) pixel:vec3<u32>) {
    let dimensions=textureDimensions(mip0);
    if any(pixel.xy>=dimensions) { return; }
    let uv=(vec2<f32>(pixel.xy)+0.5)/vec2<f32>(dimensions);
    let logical=u.bounds.zw/u.viewport.z;
    let local=uv*logical;
    let f=field(local);
    let depth=max(-f.x,0.);
    if f.x>1.5 || depth>u.reflection.y+1.5 { textureStore(mip0,vec2<i32>(pixel.xy),vec4<f32>(0.)); return; }
    let normal=f.yz;
    let boundary=local-normal*f.x;
    let distance=0.75+clamp(depth/max(u.reflection.y,0.1),0.,1.)*u.reflection_info.x;
    let position=u.bounds.xy+(boundary+normal*distance)*u.viewport.z;
    let tangent=vec2<f32>(-normal.y,normal.x);
    let sigma=mix(3.5,0.35,u.reflection_info.y)+distance*mix(0.3,0.045,u.reflection_info.y);
    let nw=max(u.reflection_info.x/(max(u.reflection.y,0.1)*u.viewport.z),sigma*2.);
    let tw=max(1./u.viewport.z,sigma*2.);
    let ruv=(position-u.crop.xy)/u.crop.zw;
    let lod=clamp(log2(max(max(nw/8.,tw)*u.viewport.z/u.reflection_info.w,1.)),0.,u.reflection_info.z);
    var value=vec4<f32>(0.);
    for(var n=0;n<4;n++) {
        let offset=normal*nw*((f32(n)+0.5)/4.-0.5)*u.viewport.z/u.crop.zw;
        value+=textureSampleLevel(radiance_pyramid,linear,ruv+offset,lod)*0.25;
    }
    // Integrate the compressed normal with four taps; neighboring mips handle
    // tangential softness. Close sources retain original scene detail.
    if distance<20. {
        var detail=vec4<f32>(0.);
        for(var n=0;n<4;n++) { for(var t=0;t<2;t++) {
            let offset=normal*nw*((f32(n)+0.5)/4.-0.5)+tangent*tw*((f32(t)+0.5)/2.-0.5);
            detail+=exterior_pixel(position+offset*u.viewport.z);
        }}
        value=mix(detail*0.125,value,smoothstep(8.,20.,distance));
    }
    let alpha=clamp(value.a*2.,0.,1.);
    textureStore(mip0,vec2<i32>(pixel.xy),vec4<f32>(value.rgb/max(value.a,0.0001)*alpha,alpha));
}

// Overlapping 1:3:3:1 Gaussian reductions avoid block-boundary energy jumps
// when a thin moving source crosses a broad mip cell. Four bilinear fetches
// integrate 16 texels; the small pyramid stays within one compute pass.
@compute @workgroup_size(8,8,1)
fn gaussian_mip(@builtin(global_invocation_id) pixel:vec3<u32>) {
    let dimensions=textureDimensions(mip0);
    if any(pixel.xy>=dimensions) { return; }
    let source_size=vec2<f32>(textureDimensions(source));
    let center=(vec2<f32>(pixel.xy)*2.+1.)/source_size;
    let o=vec2<f32>(0.75)/source_size;
    let value=(textureSampleLevel(source,linear,center+o,0.)+
               textureSampleLevel(source,linear,center-o,0.)+
               textureSampleLevel(source,linear,center+vec2<f32>(o.x,-o.y),0.)+
               textureSampleLevel(source,linear,center+vec2<f32>(-o.x,o.y),0.))*0.25;
    textureStore(mip0,vec2<i32>(pixel.xy),value);
}

// Native backdrop preparation is separate from the strict exterior edge capture.
// Never fold or mirror samples across the material boundary.
fn bleed_region(dimensions:vec2<u32>,level:u32)->vec4<u32> {
    let step=f32(1u<<level);
    let halo=5.*(step-1.);
    let lo=clamp(floor((u.bleed_damage.xy-halo)/step),vec2<f32>(0.),vec2<f32>(dimensions));
    let hi=clamp(ceil((u.bleed_damage.zw+halo)/step),lo,vec2<f32>(dimensions));
    return vec4<u32>(vec2<u32>(lo),vec2<u32>(hi));
}
@compute @workgroup_size(8,8,1)
fn bleed_capture(@builtin(global_invocation_id) global:vec3<u32>) {
    let region=bleed_region(textureDimensions(mip0),0u);
    let pixel=global.xy+region.xy;
    if any(pixel>=region.zw) { return; }
    let screen=vec2<f32>(pixel)+0.5;
    textureStore(mip0,vec2<i32>(pixel),textureSampleLevel(source,linear,screen/u.viewport.xy,0.));
}

alias BleedValue = vec4<f32>;
fn bleed_round(v:BleedValue)->BleedValue {return quantizeToF16(v);}
// Native Metal compute variant: a coarse sample grid shared by neighboring
// outputs. AIR uses half arithmetic and a 13-point stencil at integer offsets.
// No per-pixel 13 texture fetches, and no cross-dispatch/persistent history.
var<workgroup> bleed_cache: array<BleedValue,144>;
fn half_add(a:BleedValue,b:BleedValue)->BleedValue { return bleed_round(a+b); }
fn half_four(a:BleedValue,b:BleedValue,c:BleedValue,d:BleedValue)->BleedValue {
    return half_add(half_add(half_add(a,b),c),d);
}
fn bleed_sample(at:vec2<f32>,size:vec2<f32>,base:bool)->BleedValue {
    if !base { return bleed_round(BleedValue(textureSampleLevel(source,linear,(at*2.+1.)/size,0.))); }
    // Apple's base copy averages four half-precision texel reads before the
    // stencil. Bilinear sampling is close but has different rounding.
    let p=vec2<i32>(at)*2;
    let hi=vec2<i32>(size)-1;
    let tl=bleed_round(BleedValue(textureLoad(source,clamp(p,vec2<i32>(0),hi),0)));
    let tr=bleed_round(BleedValue(textureLoad(source,clamp(p+vec2<i32>(1,0),vec2<i32>(0),hi),0)));
    let bl=bleed_round(BleedValue(textureLoad(source,clamp(p+vec2<i32>(0,1),vec2<i32>(0),hi),0)));
    let br=bleed_round(BleedValue(textureLoad(source,clamp(p+vec2<i32>(1,1),vec2<i32>(0),hi),0)));
    return bleed_round(half_four(bl,br,tr,tl)*0.25);
}
@compute @workgroup_size(8,8,1)
fn bleed_mip(@builtin(global_invocation_id) global:vec3<u32>,
             @builtin(local_invocation_id) local:vec3<u32>,
             @builtin(workgroup_id) group:vec3<u32>) {
    let dimensions=textureDimensions(mip0);
    let source_size=textureDimensions(source);
    let size=vec2<f32>(source_size);
    let level=countLeadingZeros(max(source_size.x,source_size.y)) - countLeadingZeros(u32(max(u.bleed_crop.z,u.bleed_crop.w)))+1u;
    let region=bleed_region(dimensions,level);
    let pixel=global.xy+region.xy;
    let index=local.y*8u+local.x;
    for(var i=index;i<144u;i+=64u) {
        let at=vec2<f32>(region.xy+group.xy*8u)+vec2<f32>(f32(i%12u),f32(i/12u))-2.;
        bleed_cache[i]=bleed_sample(at,size,level==1u);
    }
    workgroupBarrier();
    if any(pixel>=region.zw) { return; }
    let c=(local.y+2u)*12u+local.x+2u;
    let outer=half_four(bleed_cache[c-24u],bleed_cache[c-2u],bleed_cache[c+2u],bleed_cache[c+24u]);
    let diagonal=half_four(bleed_cache[c-13u],bleed_cache[c-11u],bleed_cache[c+11u],bleed_cache[c+13u]);
    let axis=half_four(bleed_cache[c-12u],bleed_cache[c-1u],bleed_cache[c+1u],bleed_cache[c+12u]);
    let center=bleed_round(bleed_cache[c]*0.10546875);
    let d=bleed_round(diagonal*0.07708740234375);
    let a=bleed_round(axis*0.0902099609375);
    let o=bleed_round(outer*0.05633544921875);
    let value=vec4<f32>(half_add(half_add(half_add(d,center),a),o));
    // The base-mip kernel flushes tiny RGB; later downsample kernels do not.
    let rgb=select(value.rgb,vec3<f32>(0.),abs(value.rgb)<vec3<f32>(0.00010001659393310547) & vec3<bool>(level==1u));
    textureStore(mip0,vec2<i32>(pixel),vec4<f32>(rgb,value.a));
}
