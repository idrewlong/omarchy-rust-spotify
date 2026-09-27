//! `?`: every key that works in the skin you're in, in a box over it. Any
//! key closes it.

use ratatui::widgets::Clear;

use super::*;

/// The keys for `skin`, grouped: (heading, [(keys, what they do)]).
fn sections(skin: Skin) -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let mut out = vec![(
        "Playback",
        vec![
            ("space", "play / pause"),
            ("n  p", "next / previous track"),
            ("←  →", "back / forward 10 seconds"),
            ("s  r", "shuffle / repeat"),
            ("+  -", "volume"),
        ],
    )];
    if skin.uses_browser() {
        out.push((
            "Library",
            vec![
                ("/", "search"),
                ("↑ ↓  j k", "move"),
                ("enter", "play / open"),
                ("tab  h l", "playlists / songs"),
                ("backspace", "back"),
            ],
        ));
    }
    match skin {
        Skin::Ipod => out.push((
            "iPod",
            vec![
                ("↑ ↓", "turn the wheel"),
                ("enter", "select"),
                ("m  backspace", "menu"),
            ],
        )),
        Skin::Visualizer => out.push((
            "Visualizer",
            vec![("v", "next visualizer"), ("V", "open in its own window")],
        )),
        Skin::Lyrics => out.push(("Lyrics", vec![("click a line", "jump there")])),
        _ => {}
    }
    out.push((
        "Player",
        vec![
            ("t  T", "next / previous skin"),
            ("L", "sign in"),
            ("?", "this help"),
            ("q", "quit"),
        ],
    ));
    out
}

pub(super) fn draw(f: &mut Frame, app: &App) {
    let p = app.settings.palette;
    let area = f.area();
    let sections = sections(app.settings.layout.skin);
    let rows: u16 = sections
        .iter()
        .map(|(_, k)| k.len() as u16 + 2)
        .sum::<u16>()
        + 1;
    let (w, h) = (46u16.min(area.width), (rows + 2).min(area.height));
    if w < 20 || h < 6 {
        return;
    }
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(p.accent))
        .title(Span::styled(
            " Keys ",
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(" any key closes ", Style::new().fg(p.muted)).right_aligned())
        .style(Style::new().fg(p.fg).bg(p.bg));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = Vec::new();
    for (i, (title, keys)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            *title,
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
        for (k, what) in keys {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {k:<14}"),
                    Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(*what, Style::new().fg(p.muted)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}
