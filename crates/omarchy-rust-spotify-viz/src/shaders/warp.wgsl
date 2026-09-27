// Warp: flying through a starfield. The bass is the throttle; stars take
// their colour from the part of the spectrum they're tied to and flare
// with it; beats flash the centre.

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let p = centered(uv);
    // Streaks: the last frame pushed outward and dimmed.
    // (Small steps: bigger ones leave the streaks dotted.)
    var col = prev(uncentered(p * 0.988)) * 0.88;
    for (var layer = 0; layer < 5; layer++) {
        let depth = fract(f32(layer) / 5.0 + travel() * 0.25);
        let scale = mix(28.0, 0.8, depth);
        let fade = smoothstep(0.0, 0.25, depth) * smoothstep(1.0, 0.8, depth);
        let g = p * scale + vec2<f32>(f32(layer) * 17.3, f32(layer) * 5.1);
        let id = floor(g);
        let h = hash(id);
        if (h > 0.82) {
            let off = vec2<f32>(hash(id + 7.0), hash(id + 13.0)) - 0.5;
            let d = length(fract(g) - 0.5 - off * 0.6);
            let s = spectrum(fract(h * 7.0));
            let star = smoothstep(0.09 + s * 0.08, 0.0, d);
            col += hsv(fract(h * 7.0) * 280.0 + 180.0, 0.55, 1.0) * star * fade * (0.5 + s * 1.5);
        }
    }
    col += vec3<f32>(0.6, 0.7, 1.0) * beat() * 0.25 * smoothstep(0.25, 0.0, length(p));
    return col;
}
