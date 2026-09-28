// Battery: a feedback tunnel. Every frame is the last one zoomed in,
// turned and mirrored left to right, dimmed a little; the waveform is drawn
// on top as a ring whose size follows the bass. The spin reverses every
// eight beats and the colours jump on each one.

fn scene(uv: vec2<f32>) -> vec3<f32> {
    let p = centered(uv);
    let zoom = 1.012 + bass() * 0.03 + beat() * 0.025;
    let dir = select(1.0, -1.0, (i32(beats()) / 8) % 2 == 1);
    var q = rot((0.004 + treble() * 0.014) * dir) * (p / zoom);
    q.x = -abs(q.x);
    let back = prev(uncentered(q)) * (0.925 + mid() * 0.05);

    // Around the ring, there and back, so the wave meets itself.
    let a = atan2(p.y, p.x) / TAU + 0.5;
    let x = 1.0 - abs(2.0 * fract(a + time() * 0.03) - 1.0);
    let r = 0.16 + bass() * 0.12 + wave(x) * 0.09;
    let d = abs(length(p) - r);
    let line = smoothstep(0.006, 0.0, d) + 0.3 * smoothstep(0.04, 0.0, d);
    let col = hsv(time() * 25.0 + x * 300.0 + beats() * 47.0, 0.85, 1.0);
    return back + col * line;
}
