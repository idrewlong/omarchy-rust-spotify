// Spectrum: Bars and Waves in high definition. Glowing bars on a mirror
// floor with falling afterglow, and the waveform drawn across the top.

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let bars = 64.0;
    let cell = floor(uv.x * bars);
    let cx = (cell + 0.5) / bars;
    let v = pow(spectrum(cx), 1.3);
    let in_bar = smoothstep(0.42, 0.34, abs(fract(uv.x * bars) - 0.5));
    let floor_y = 0.74;
    let h = v * 0.58;
    let y = floor_y - uv.y;
    let hue = 190.0 + cx * 170.0 + time() * 6.0;

    var col = vec3<f32>(0.0);
    if (y > 0.0 && y < h) {
        col = hsv(hue, 0.8, 0.35 + 0.65 * y / max(h, 0.001)) * in_bar;
    }
    // The reflection, fading away from the floor.
    if (y < 0.0 && -y < h * 0.6) {
        col = hsv(hue, 0.8, 0.3) * in_bar * (1.0 + y / (h * 0.6));
    }
    // A hot edge on each bar's top.
    col += hsv(hue - 20.0, 0.5, 1.0) * exp(-abs(y - h) * 90.0) * in_bar * step(0.0, y);

    // The scope.
    let w = wave(uv.x) * 0.1;
    let d = abs(uv.y - (0.22 - w));
    col += hsv(time() * 30.0, 0.35, 1.0) * (smoothstep(0.004, 0.0, d) + 0.3 * smoothstep(0.03, 0.0, d));

    // Afterglow that drifts upward above the floor.
    // (Only below the scope, so it doesn't smear the waveform.)
    let back = prev(uv + vec2<f32>(0.0, 0.004)) * 0.86 * step(0.0, y) * smoothstep(0.3, 0.36, uv.y);
    return max(col, back);
}
