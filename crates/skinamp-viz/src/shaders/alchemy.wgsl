// Alchemy: domain-warped noise, a slowly folding field of colour. The mids
// set the speed, the bass the brightness and warp, the highs add sparks
// along the folds; every eight beats a new scene blends in.

fn noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(hash(i), hash(i + vec2<f32>(1.0, 0.0)), s.x),
        mix(hash(i + vec2<f32>(0.0, 1.0)), hash(i + vec2<f32>(1.0, 1.0)), s.x),
        s.y,
    );
}

fn fbm(p0: vec2<f32>) -> f32 {
    var p = p0;
    var v = 0.0;
    var a = 0.5;
    for (var i = 0; i < 5; i++) {
        v += a * noise(p);
        p = rot(0.5) * p * 2.02;
        a *= 0.5;
    }
    return v;
}

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let p = centered(uv) * 2.4;
    let seed = floor(beats() / 8.0);
    let t = time() * 0.12 + travel() * 0.15;
    let q = vec2<f32>(fbm(p + vec2<f32>(seed * 1.7, t)), fbm(p + vec2<f32>(5.2 + seed, -t * 0.8)));
    let r = vec2<f32>(
        fbm(p + 3.0 * q + vec2<f32>(1.7, 9.2) + t * 0.5),
        fbm(p + 3.0 * q + vec2<f32>(8.3, 2.8) - t * 0.4),
    );
    let f = fbm(p + (3.0 + bass() * 1.5) * r);
    let hue = seed * 67.0 + f * 240.0 + time() * 6.0;
    var col = hsv(hue, 0.75, pow(f, 1.4) * 1.9);
    col *= 0.55 + 0.6 * bass() + 0.4 * beat();
    col += hsv(hue + 140.0, 0.5, 1.0) * pow(length(q) * 0.9, 4.0) * treble();
    // Blend with the last frame, so scene changes melt rather than cut.
    return mix(prev(uv), col, 0.2);
}
