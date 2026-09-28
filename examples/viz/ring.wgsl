// A starting point for your own preset. Copy it to
// ~/.config/skinamp/viz/ring.wgsl, open the visualizer
// (skinamp-viz, or V in the player's visualizer skin), pick
// "ring" with the arrow keys, and edit: it reloads each time you save.
//
// A preset is one function returning each pixel's colour. The helpers
// (bass(), spectrum(x), wave(x), prev(uv), hsv(), ...) are listed at the top
// of crates/skinamp-viz/src/shaders/prelude.wgsl.

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let p = centered(uv);
    // The ring's radius follows the bass; its edge wobbles with the waveform.
    let a = atan2(p.y, p.x) / TAU + 0.5;
    let r = 0.12 + bass() * 0.25 + wave(1.0 - abs(2.0 * a - 1.0)) * 0.05;
    let d = abs(length(p) - r);
    let ring = hsv(time() * 40.0 + beats() * 30.0, 0.8, 1.0) * smoothstep(0.015, 0.0, d);
    // Plus the last frame, slightly zoomed and dimmed: trails.
    return ring + prev(uncentered(p * 0.98)) * 0.9;
}
