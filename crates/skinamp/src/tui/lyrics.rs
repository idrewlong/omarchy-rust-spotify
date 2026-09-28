//! "lyrics": the words of what's playing, in your Omarchy theme. Synced
//! lyrics follow the song: the current line in the accent colour at the
//! middle of the screen, the lines around it fading out; click a line to
//! jump there. Unsynced lyrics scroll along with the song. Lyrics come from
//! LRCLIB through the daemon (`Request::Lyrics`), fetched when this skin is
//! showing and the track changes.

use ratatui::buffer::Buffer;
use serde_json::Value;
use skinamp_proto::Request;

use super::paint::text;
use super::*;

#[derive(Default)]
pub(super) enum State {
    #[default]
    Idle,
    Loading,
    Ready {
        synced: bool,
        instrumental: bool,
        lines: Vec<(u32, String)>,
        provider: String,
    },
    Failed,
}

#[derive(Default)]
pub(super) struct Lyrics {
    /// The track the state is for, and the request in flight for it.
    pub(super) for_uri: Option<String>,
    pub(super) req: Option<u64>,
    next_id: u64,
    pub(super) state: State,
}

impl Lyrics {
    /// The request to send, if this track's lyrics haven't been asked for.
    pub(super) fn wanted(&mut self, uri: Option<&str>) -> Option<(u64, Request)> {
        let uri = uri?;
        if self.for_uri.as_deref() == Some(uri) {
            return None;
        }
        self.for_uri = Some(uri.to_string());
        // Ids of their own, clear of the library browser's.
        self.next_id += 1;
        let id = 5_000_000 + self.next_id;
        self.req = Some(id);
        self.state = State::Loading;
        Some((
            id,
            Request::Lyrics {
                uri: uri.to_string(),
            },
        ))
    }

    pub(super) fn on_json(&mut self, value: &Value) {
        let lines = value["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|l| {
                (
                    l["ms"].as_u64().unwrap_or(0) as u32,
                    l["text"].as_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        self.state = State::Ready {
            synced: value["synced"].as_bool().unwrap_or(false),
            instrumental: value["instrumental"].as_bool().unwrap_or(false),
            lines,
            provider: value["provider"].as_str().unwrap_or("").to_string(),
        };
        self.req = None;
    }

    pub(super) fn on_error(&mut self) {
        self.state = State::Failed;
        self.req = None;
    }
}

/// The line playing at `pos` (the last one started), a beat early so the
/// highlight lands with the singing rather than after it.
fn current(lines: &[(u32, String)], pos: u32) -> usize {
    let at = pos + 250;
    lines.iter().rposition(|(ms, _)| *ms <= at).unwrap_or(0)
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let p = app.settings.palette;
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().fg(p.fg).bg(p.bg)), area);
    if area.width < 20 || area.height < 8 {
        return;
    }
    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    let buf = f.buffer_mut();
    let st = |c: Color| Style::new().fg(c).bg(p.bg);

    // Title and hint.
    let title = match (&s.track, banner(app)) {
        (_, Some(b)) => b,
        (Some(t), None) => format!("{}  ·  {}", t.name, t.artists.join(", ")),
        (None, None) => "Nothing playing".into(),
    };
    text(
        buf,
        area.x + 2,
        area.y + 1,
        area.width - 4,
        &title,
        st(p.fg).add_modifier(Modifier::BOLD),
    );
    let hint = "lyrics · t next";
    text(
        buf,
        area.right().saturating_sub(hint.len() as u16 + 2),
        area.y + 1,
        hint.len() as u16,
        hint,
        st(p.muted),
    );

    let field = Rect {
        x: area.x + 2,
        y: area.y + 3,
        width: area.width - 4,
        height: area.height - 6,
    };
    let mid = field.y + field.height / 2;
    let centered = |buf: &mut Buffer, y: u16, line: &str, style: Style| {
        let line = super::paint::wrap(line, field.width as usize, 1)
            .pop()
            .unwrap_or_default();
        let w = super::paint::width(&line);
        text(
            buf,
            field.x + field.width.saturating_sub(w) / 2,
            y,
            w,
            &line,
            style,
        );
    };
    let pos = s.position_now_ms();
    let dur = s.track.as_ref().map(|t| t.duration_ms).unwrap_or(0);

    match (&s.track, &app.lyrics.state) {
        (None, _) => {}
        (Some(_), State::Idle | State::Loading) => {
            centered(buf, mid, "Finding the lyrics…", st(p.muted));
        }
        (Some(_), State::Failed) => {
            centered(buf, mid, "Couldn't reach the lyrics service", st(p.muted));
        }
        (
            Some(_),
            State::Ready {
                instrumental: true, ..
            },
        ) => {
            centered(buf, mid, "♪  Instrumental  ♪", st(p.muted));
        }
        (Some(_), State::Ready { lines, .. }) if lines.iter().all(|(_, t)| t.is_empty()) => {
            centered(buf, mid, "No lyrics found for this song", st(p.muted));
        }
        (
            Some(_),
            State::Ready {
                synced,
                lines,
                provider,
                ..
            },
        ) => {
            // The line at the middle: the current one when synced, else
            // where the song is, as a share of the lyrics.
            let at = if *synced {
                current(lines, pos)
            } else if dur > 0 {
                (lines.len() as u64 * pos as u64 / dur as u64) as usize
            } else {
                0
            }
            .min(lines.len().saturating_sub(1));
            for y in field.top()..field.bottom() {
                let i = at as i64 + y as i64 - mid as i64;
                let Some((ms, words)) = usize::try_from(i).ok().and_then(|i| lines.get(i)) else {
                    continue;
                };
                let d = (y as i64 - mid as i64).unsigned_abs();
                let style = if !*synced {
                    st(p.fg)
                } else if d == 0 {
                    st(p.accent).add_modifier(Modifier::BOLD)
                } else if d <= 2 {
                    st(p.fg)
                } else {
                    st(p.muted)
                };
                let shown = if words.is_empty() {
                    "♪"
                } else {
                    words.as_str()
                };
                centered(buf, y, shown, style);
                if *synced && !words.is_empty() {
                    clicks.push((
                        Rect {
                            y,
                            height: 1,
                            ..field
                        },
                        Hit::Cmd(Command::Seek { ms: *ms }),
                    ));
                }
            }
            if !provider.is_empty() {
                let credit = format!("lyrics: {provider}");
                let w = credit.len() as u16;
                text(
                    buf,
                    area.right().saturating_sub(w + 2),
                    area.bottom() - 3,
                    w,
                    &credit,
                    st(p.muted),
                );
            }
        }
    }

    // Progress and time at the bottom.
    let times = format!("{} / {}", fmt_ms(pos), fmt_ms(dur));
    let by = area.bottom() - 2;
    let bar = Rect {
        x: area.x + 2,
        y: by,
        width: area.width.saturating_sub(6 + times.len() as u16),
        height: 1,
    };
    let ratio = if dur > 0 {
        (pos as f64 / dur as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (bar.width as f64 * ratio).round() as u16;
    for i in 0..bar.width {
        let (sym, c) = if i < filled {
            ("━", p.accent)
        } else {
            ("─", p.muted)
        };
        buf[(bar.x + i, by)].set_symbol(sym).set_fg(c).set_bg(p.bg);
    }
    text(
        buf,
        bar.right() + 2,
        by,
        times.len() as u16,
        &times,
        st(p.muted),
    );
    if s.track.is_some() {
        clicks.push((bar, Hit::Seek));
    }
    app.hits.extend(clicks);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_song() {
        let lines: Vec<(u32, String)> = [(0, "a"), (19_050, "b"), (23_660, "c")]
            .into_iter()
            .map(|(m, t)| (m, t.to_string()))
            .collect();
        assert_eq!(current(&lines, 0), 0);
        assert_eq!(current(&lines, 18_900), 1, "a beat early");
        assert_eq!(current(&lines, 20_000), 1);
        assert_eq!(current(&lines, 99_000), 2);
    }

    #[test]
    fn asks_once_per_track() {
        let mut l = Lyrics::default();
        assert!(l.wanted(None).is_none());
        let (id, _) = l.wanted(Some("spotify:track:x")).unwrap();
        assert!(l.wanted(Some("spotify:track:x")).is_none());
        assert!(l.wanted(Some("spotify:track:y")).is_some());
        assert_ne!(Some(id), l.req);
    }
}
