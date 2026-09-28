// Shared by every preset. A preset is one function:
//
//     fn scene(uv: vec2<f32>) -> vec3<f32>
//
// called for every pixel, uv from (0,0) top-left to (1,1) bottom-right,
// returning its colour (0..1). What it can read:
//
//   time(), dt()          seconds, and since the last frame
//   bass(), mid(), treble()   loudness 0..1, smoothed
//   beat()                1.0 on a kick, fading over ~150 ms
//   beats()               kicks so far (for changing scenes every n)
//   travel()              distance travelled: grows faster with the bass
//   spectrum(x)           the spectrum at x in 0..1 (lows to highs), 0..1
//   wave(x)               the waveform at x in 0..1, -1..1
//   prev(uv)              last frame's colour here: feedback
//   aspect()              width / height
//   centered(uv)          uv with (0,0) in the middle and square units
//   hsv(h, s, v)          colour from hue in degrees
//   rot(a)                a 2D rotation matrix

struct U {
    res: vec4<f32>,      // width, height, aspect, frame
    clock: vec4<f32>,    // time, dt, beat pulse, beats
    levels: vec4<f32>,   // bass, mid, treble, overall
    motion: vec4<f32>,   // travel, unused x3
    spectrum: array<vec4<f32>, 12>,
    wave: array<vec4<f32>, 64>,
}

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var prev_tex: texture_2d<f32>;
@group(0) @binding(2) var prev_samp: sampler;

const PI: f32 = 3.14159265;
const TAU: f32 = 6.28318531;

fn time() -> f32 { return u.clock.x; }
fn dt() -> f32 { return u.clock.y; }
fn beat() -> f32 { return u.clock.z; }
fn beats() -> f32 { return u.clock.w; }
fn bass() -> f32 { return u.levels.x; }
fn mid() -> f32 { return u.levels.y; }
fn treble() -> f32 { return u.levels.z; }
fn loudness() -> f32 { return u.levels.w; }
fn aspect() -> f32 { return u.res.z; }
fn travel() -> f32 { return u.motion.x; }

fn band(i: i32) -> f32 {
    let j = clamp(i, 0, 47);
    return u.spectrum[j / 4][j % 4];
}

fn spectrum(x: f32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * 47.0;
    let i = i32(floor(f));
    return mix(band(i), band(i + 1), fract(f));
}

fn sample_wave(i: i32) -> f32 {
    let j = clamp(i, 0, 255);
    return u.wave[j / 4][j % 4];
}

fn wave(x: f32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * 255.0;
    let i = i32(floor(f));
    return mix(sample_wave(i), sample_wave(i + 1), fract(f));
}

fn prev(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(prev_tex, prev_samp, uv, 0.0).rgb;
}

fn centered(uv: vec2<f32>) -> vec2<f32> {
    return vec2<f32>((uv.x - 0.5) * aspect(), uv.y - 0.5);
}

fn uncentered(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(p.x / aspect() + 0.5, p.y + 0.5);
}

fn hsv(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(1.0, 2.0 / 3.0, 1.0 / 3.0);
    let p = abs(fract(vec3<f32>(h / 360.0) + k) * 6.0 - vec3<f32>(3.0));
    return v * mix(vec3<f32>(1.0), clamp(p - vec3<f32>(1.0), vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

fn rot(a: f32) -> mat2x2<f32> {
    let c = cos(a);
    let s = sin(a);
    return mat2x2<f32>(c, s, -s, c);
}

fn hash(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

struct VOut {
    @builtin(position) pos: vec4<f32>,
}

// One triangle that covers the screen.
@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return VOut(vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0));
}

@fragment
fn fs_main(v: VOut) -> @location(0) vec4<f32> {
    let uv = v.pos.xy / u.res.xy;
    return vec4<f32>(clamp(scene(uv), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
