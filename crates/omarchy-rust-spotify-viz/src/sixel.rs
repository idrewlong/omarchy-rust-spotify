//! RGBA to Sixel, fast enough for 30 frames a second: a fixed 6x7x6 colour
//! cube (252 colours) with 4x4 ordered dithering, so there's no palette to
//! compute per frame and gradients stay smooth, and run-length encoding.
//!
//! Each band of six rows is walked once, column by column, collecting per
//! colour the columns it appears in; each colour's line is then written
//! from that list. The work follows the pixels, not colours x width.

const R: usize = 6;
const G: usize = 7;
const B: usize = 6;
const COLOURS: usize = R * G * B;

const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// level[value][threshold]: the dithered level of a channel value, for one
/// channel's number of levels.
fn table(levels: usize) -> Vec<[u8; 16]> {
    (0..256)
        .map(|v| {
            std::array::from_fn(|t| {
                let x = v as f32 / 255.0 * (levels - 1) as f32 + t as f32 / 16.0;
                (x as usize).min(levels - 1) as u8
            })
        })
        .collect()
}

/// The dithering tables for the three channels.
struct Tables {
    r: Vec<[u8; 16]>,
    g: Vec<[u8; 16]>,
    b: Vec<[u8; 16]>,
}

/// One worker's buffers, reused frame to frame.
#[derive(Default)]
struct Scratch {
    /// Per colour, (column, sixel bits) in column order, for this band.
    runs: Vec<Vec<(u16, u8)>>,
    used: Vec<usize>,
    out: Vec<u8>,
}

pub struct Encoder {
    tables: Tables,
    header: Vec<u8>,
    /// Bands are encoded in parallel, a contiguous share per worker.
    workers: Vec<Scratch>,
}

impl Default for Encoder {
    fn default() -> Self {
        let mut header = Vec::new();
        for i in 0..COLOURS {
            let (r, g, b) = (i / (G * B), (i / B) % G, i % B);
            header.extend_from_slice(
                format!(
                    "#{i};2;{};{};{}",
                    r * 100 / (R - 1),
                    g * 100 / (G - 1),
                    b * 100 / (B - 1)
                )
                .as_bytes(),
            );
        }
        let n = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
        Encoder {
            tables: Tables {
                r: table(R),
                g: table(G),
                b: table(B),
            },
            header,
            workers: (0..n)
                .map(|_| Scratch {
                    runs: vec![Vec::new(); COLOURS],
                    ..Default::default()
                })
                .collect(),
        }
    }
}

fn push_num(out: &mut Vec<u8>, mut n: usize) {
    // Digits into a stack buffer: this runs tens of thousands of times a
    // frame, so no allocation.
    let mut d = [0u8; 20];
    let mut i = d.len();
    loop {
        i -= 1;
        d[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&d[i..]);
}

/// `run` repeats of the sixel `ch`, compressed when that's shorter.
fn emit(out: &mut Vec<u8>, ch: u8, run: usize) {
    if run > 3 {
        out.push(b'!');
        push_num(out, run);
        out.push(ch);
    } else {
        out.extend(std::iter::repeat_n(ch, run));
    }
}

impl Encoder {
    /// Appends the Sixel image for `rgba` (`w` x `h`, packed rows) to `out`.
    pub fn encode(&mut self, rgba: &[u8], w: usize, h: usize, out: &mut Vec<u8>) {
        // DCS, pixel aspect 1:1, raster size, the palette.
        out.extend_from_slice(b"\x1bP0;1;0q\"1;1;");
        push_num(out, w);
        out.push(b';');
        push_num(out, h);
        out.extend_from_slice(&self.header);
        let bands = h.div_ceil(6);
        let per = bands.div_ceil(self.workers.len()).max(1);
        let tables = &self.tables;
        std::thread::scope(|scope| {
            for (i, s) in self.workers.iter_mut().enumerate() {
                let first = (i * per).min(bands);
                let last = ((i + 1) * per).min(bands);
                s.out.clear();
                scope.spawn(move || {
                    for band in first..last {
                        encode_band(tables, s, rgba, w, h, band * 6);
                    }
                });
            }
        });
        for s in &self.workers {
            out.extend_from_slice(&s.out);
        }
        out.extend_from_slice(b"\x1b\\");
    }
}

/// The six rows from `y0` into `s.out`, ending with the band's newline.
fn encode_band(t: &Tables, s: &mut Scratch, rgba: &[u8], w: usize, h: usize, y0: usize) {
    let rows = 6.min(h - y0);
    for x in 0..w {
        let tx = x % 4;
        for row in 0..rows {
            let y = y0 + row;
            let th = BAYER[y % 4][tx] as usize;
            let p = (y * w + x) * 4;
            let c = t.r[rgba[p] as usize][th] as usize * (G * B)
                + t.g[rgba[p + 1] as usize][th] as usize * B
                + t.b[rgba[p + 2] as usize][th] as usize;
            let runs = &mut s.runs[c];
            match runs.last_mut() {
                Some((lx, bits)) if *lx as usize == x => *bits |= 1 << row,
                _ => {
                    if runs.is_empty() {
                        s.used.push(c);
                    }
                    runs.push((x as u16, 1 << row));
                }
            }
        }
    }
    let out = &mut s.out;
    for (k, &c) in s.used.iter().enumerate() {
        if k > 0 {
            out.push(b'$');
        }
        out.push(b'#');
        push_num(out, c);
        let runs = &s.runs[c];
        let mut cursor = 0usize;
        let mut i = 0;
        while i < runs.len() {
            let (x, bits) = runs[i];
            let x = x as usize;
            if x > cursor {
                // Columns this colour skips.
                emit(out, 63, x - cursor);
            }
            // A run: the same bits in consecutive columns.
            let mut n = 1;
            while i + n < runs.len() && runs[i + n].0 as usize == x + n && runs[i + n].1 == bits {
                n += 1;
            }
            emit(out, 63 + bits, n);
            cursor = x + n;
            i += n;
        }
    }
    for &c in &s.used {
        s.runs[c].clear();
    }
    s.used.clear();
    out.push(b'-');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_small_image() {
        // 3x7: two bands; top-left pixel white, the rest black.
        let mut px = vec![0u8; 3 * 7 * 4];
        px[..4].copy_from_slice(&[255, 255, 255, 255]);
        let mut out = Vec::new();
        Encoder::default().encode(&px, 3, 7, &mut out);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1bP0;1;0q\"1;1;3;7"));
        assert!(s.ends_with("-\x1b\\"));
        // White is the last colour; its first column has bit 0 set.
        assert!(s.contains(&format!("#{}@", COLOURS - 1)), "{s}");
        // Black fills the rest of band 0: column 0 without the top pixel
        // (bits 111110 = 62, '}'), then two full columns ('~~').
        assert!(s.contains("#0}~~"), "{s}");
        assert_eq!(s.matches('-').count(), 2);
    }

    #[test]
    fn gaps_and_runs_are_compressed() {
        // One row, 10 wide: white at x = 0 and x = 6..10, black between.
        let mut px = vec![0u8; 10 * 4];
        for x in [0, 6, 7, 8, 9] {
            px[x * 4..x * 4 + 4].copy_from_slice(&[255; 4]);
        }
        let mut out = Vec::new();
        Encoder::default().encode(&px, 10, 1, &mut out);
        let s = String::from_utf8(out).unwrap();
        // White: one column, a five-column gap, then a run of four.
        assert!(s.contains(&format!("#{}@!5?!4@", COLOURS - 1)), "{s}");
    }

    #[test]
    fn a_frame_encodes_quickly() {
        // A noisy 1280x720 frame, the worst case for runs.
        let (w, h) = (1280, 720);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i * 2654435761usize >> 7) as u8).collect();
        let mut enc = Encoder::default();
        let mut out = Vec::new();
        enc.encode(&px, w, h, &mut out);
        let start = std::time::Instant::now();
        for _ in 0..5 {
            out.clear();
            enc.encode(&px, w, h, &mut out);
        }
        let per = start.elapsed() / 5;
        eprintln!("1280x720 noise: {per:?} per frame, {} KB", out.len() / 1024);
    }
}
