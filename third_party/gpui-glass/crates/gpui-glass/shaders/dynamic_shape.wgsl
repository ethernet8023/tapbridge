// Regenerate a signed-distance field from freshly evaluated Lisse contour segments.
// No affine warping of a previous capsule. All coordinates are logical pixels.
struct Input { info: vec4<f32>, segments: array<vec4<f32>> };
@group(0) @binding(0) var<storage,read> input: Input;
@group(0) @binding(1) var output: texture_storage_2d<rgba16float,write>;
@compute @workgroup_size(8,8)
fn build(@builtin(global_invocation_id) id:vec3<u32>) {
    let dimensions=textureDimensions(output);
    if any(id.xy>=dimensions) { return; }
    let p=(vec2<f32>(id.xy)+0.5)/2.-input.info.y;
    var distance2=1e20;var closest=vec2<f32>(0.);var inside=false;
    var direction=vec2<f32>(0.);var weight_sum=0.;
    for(var i=0u;i<u32(input.info.x);i+=1u) {
        let segment=input.segments[i];let a=segment.xy;let b=segment.zw;
        let edge=b-a;let len=max(length(edge),0.00001);
        let t=clamp(dot(p-a,edge)/(len*len),0.,1.);
        let delta=p-(a+t*edge);let d2=dot(delta,delta);
        let normal=vec2<f32>(edge.y,-edge.x)/len;
        if d2<distance2 {distance2=d2;closest=normal;}
        if (a.y>p.y)!=(b.y>p.y) {
            if p.x<(b.x-a.x)*(p.y-a.y)/(b.y-a.y)+a.x {inside=!inside;}
        }
        // Continuous interior normal extension. Opposing directions cancel at
        // medial axes instead of switching abruptly when the nearest edge changes.
        let weight=len/((d2+0.25)*(d2+0.25));
        direction+=normal*weight;weight_sum+=weight;
    }
    let distance=sqrt(distance2)*select(1.,-1.,inside);
    let normal=select(closest,direction/max(weight_sum,0.00001),inside);
    textureStore(output,vec2<i32>(id.xy),vec4<f32>(distance,normal,1.));
}
