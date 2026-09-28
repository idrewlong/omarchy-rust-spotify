// Ambience: slow liquid swirl. The last frame is carried along a whirlpool
// flow and ripples; the spectrum wraps a circle and bleeds colour into it.

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let p = centered(uv);
    let r = length(p);
    var q = rot((0.012 + 0.02 * mid()) * (1.2 - r)) * p * (0.992 - bass() * 0.012);
    q += 0.0025 * vec2<f32>(sin(p.y * 9.0 + time() * 1.3), cos(p.x * 9.0 - time()));
    var col = prev(uncentered(q)) * 0.972;

    let a = atan2(p.y, p.x);
    let x = abs(a) / PI;
    let s = spectrum(x);
    let d = abs(r - (0.1 + s * 0.3));
    col += hsv(time() * 12.0 + x * 220.0, 0.7, 1.0) * smoothstep(0.012, 0.0, d) * (0.3 + s);
    // A brief flash at the centre on each kick (small: feedback adds it up).
    col += vec3<f32>(0.9, 0.95, 1.0) * beat() * beat() * 0.04 * smoothstep(0.06, 0.0, r);
    return col;
}
