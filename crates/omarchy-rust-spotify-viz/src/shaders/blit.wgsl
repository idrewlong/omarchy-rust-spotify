// The finished frame to the window, with a soft glow from a few taps of
// its blurred neighbourhood (a cheap bloom) and fading while paused.

@group(0) @binding(0) var<uniform> fade: vec4<f32>;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return VOut(vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0), vec2<f32>(p.x, 1.0 - p.y));
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(src));
    let c = textureSampleLevel(src, samp, v.uv, 0.0).rgb;
    var glow = vec3<f32>(0.0);
    for (var i = 0; i < 8; i++) {
        let a = f32(i) * 0.785398;
        let d = vec2<f32>(cos(a), sin(a)) * texel * 6.0;
        glow += textureSampleLevel(src, samp, v.uv + d, 0.0).rgb;
        glow += textureSampleLevel(src, samp, v.uv + d * 2.5, 0.0).rgb;
    }
    glow /= 16.0;
    let bloom = max(glow - vec3<f32>(0.35), vec3<f32>(0.0)) * 0.9;
    return vec4<f32>((c + bloom) * fade.x, 1.0);
}
