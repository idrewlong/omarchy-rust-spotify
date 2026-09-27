//! The default skin: centered text and progress on the Omarchy theme, with
//! the cover beside or above.

use super::*;

/// Where the cover goes, and the rect left for everything else.
fn place_cover(app: &App, body: Rect, text_rows: u16) -> (Option<Rect>, Rect) {
    let lay = &app.settings.layout;
    if app.cover.is_none() || lay.cover == CoverPlacement::None || body.height < 6 {
        return (None, body);
    }
    // Cells are taller than wide: a square cover is wider in columns.
    let font = app.picker.font_size();
    let (fw, fh) = (font.width, font.height);
    let cols_for = |rows: u16| (rows as u32 * fh.max(1) as u32 / fw.max(1) as u32) as u16;
    let want = |fits: u16| {
        let fits = fits.min(24);
        if lay.cover_size > 0 {
            lay.cover_size.min(fits)
        } else {
            fits
        }
    };

    // Beside the text, if there's room for the text too.
    if lay.cover == CoverPlacement::Left {
        let rows = want(body.height.saturating_sub(2));
        let cols = cols_for(rows);
        if rows >= 4 && body.width >= cols + 36 {
            let top = body.y + (body.height - rows) / 2;
            let cover = Rect {
                x: body.x + 2,
                y: top,
                width: cols,
                height: rows,
            };
            let rest_x = cover.x + cols + 3;
            let rest = Rect {
                x: rest_x,
                y: body.y,
                width: body.right().saturating_sub(rest_x + 1),
                height: body.height,
            };
            return (Some(cover), rest);
        }
    }
    // Above the text: leave room for text, progress, time and status.
    let rows = want(body.height.saturating_sub(text_rows + 6));
    let cols = cols_for(rows).min(body.width);
    if rows < 4 {
        return (None, body);
    }
    let cover = Rect {
        x: body.x + (body.width - cols) / 2,
        y: body.y + 1,
        width: cols,
        height: rows,
    };
    let rest = Rect {
        x: body.x,
        y: cover.bottom(),
        width: body.width,
        height: body.bottom() - cover.bottom(),
    };
    (Some(cover), rest)
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let p = app.settings.palette;
    let lay = &app.settings.layout;
    let base = Style::new().fg(p.fg).bg(p.bg);
    let area = f.area();
    f.render_widget(Block::new().style(base), area);

    let (borders, border_type) = match lay.frame {
        Frame_::None => (Borders::NONE, BorderType::Plain),
        Frame_::Rounded => (Borders::ALL, BorderType::Rounded),
        Frame_::Plain => (Borders::ALL, BorderType::Plain),
        Frame_::Double => (Borders::ALL, BorderType::Double),
        Frame_::Thick => (Borders::ALL, BorderType::Thick),
    };
    let block = Block::new()
        .borders(borders)
        .border_type(border_type)
        .border_style(Style::new().fg(p.muted))
        .title(Span::styled(
            lay.title.clone(),
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ))
        .style(base);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let align = match lay.align {
        Align::Center => Alignment::Center,
        Align::Left => Alignment::Left,
    };
    let s = &app.state;
    let mut lines: Vec<Line> = Vec::new();

    let banner = banner(app);
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();
    if let Some(b) = banner {
        lines.push(Line::styled(
            b,
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::raw(""));
    }

    match &s.track {
        Some(t) => {
            lines.push(Line::styled(
                t.name.clone(),
                Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                t.artists.join(", "),
                Style::new().fg(p.accent),
            ));
            if lay.show_album && !t.album.is_empty() {
                lines.push(Line::styled(t.album.clone(), Style::new().fg(p.muted)));
            }
        }
        None => {
            lines.push(Line::styled(
                "Nothing playing",
                Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!(
                    "Pick \"{}\" in any Spotify app, or press space",
                    s.device_name
                ),
                Style::new().fg(p.muted),
            ));
        }
    }

    // Help on the bottom row; the cover, then text, progress and status in
    // the rest.
    let text_h = lines.len() as u16;
    let [body, help_row] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(if lay.show_help { 1 } else { 0 }),
    ])
    .areas(inner);
    let (cover_rect, content) = if s.track.is_some() {
        place_cover(app, body, text_h)
    } else {
        (None, body)
    };
    let rows = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(text_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(if lay.show_status { 1 } else { 0 }),
        Constraint::Fill(1),
    ])
    .split(content);

    f.render_widget(Paragraph::new(lines).alignment(align), rows[1]);

    if let Some(t) = &s.track {
        let pos = s.position_now_ms();
        let ratio = if t.duration_ms > 0 {
            pos as f64 / t.duration_ms as f64
        } else {
            0.0
        };
        let bar_w = content.width.saturating_sub(4).min(60);
        let x = match lay.align {
            Align::Center => content.x + (content.width - bar_w) / 2,
            Align::Left => content.x,
        };
        let bar = Rect {
            x,
            y: rows[3].y,
            width: bar_w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(progress_line(bar_w, ratio, lay.progress, &p)),
            bar,
        );
        clicks.push((bar, Hit::Seek));
        let times = format!("{}  /  {}", fmt_ms(pos), fmt_ms(t.duration_ms));
        f.render_widget(
            Paragraph::new(Line::styled(times, Style::new().fg(p.muted))).alignment(align),
            rows[4],
        );
    }

    if lay.show_status {
        let play = match s.status {
            Status::Playing => "▶ playing",
            Status::Paused => "⏸ paused",
            Status::Loading => "… loading",
            Status::Stopped => "■ stopped",
        };
        let repeat = match s.repeat {
            Repeat::Off => "repeat off",
            Repeat::Context => "repeat all",
            Repeat::Track => "repeat one",
        };
        let on = |b: bool| if b { p.accent } else { p.muted };
        let status = Line::from(vec![
            Span::styled(play, Style::new().fg(p.fg)),
            Span::styled("   shuffle", Style::new().fg(on(s.shuffle))),
            Span::styled(
                format!("   {repeat}"),
                Style::new().fg(on(s.repeat != Repeat::Off)),
            ),
            Span::styled(format!("   vol {}%", s.volume), Style::new().fg(p.muted)),
            Span::styled(format!("   {}", s.device_name), Style::new().fg(p.muted)),
        ]);
        f.render_widget(Paragraph::new(status).alignment(align), rows[5]);
    }

    if lay.show_help {
        let help = match &app.settings.problem {
            Some(problem) => Line::styled(problem.clone(), Style::new().fg(p.accent)),
            None => Line::styled(
                "space play/pause · n/p next/prev · ←/→ seek · s shuffle · r repeat · +/- volume · t skin · L sign in · q quit",
                Style::new().fg(p.muted),
            ),
        };
        f.render_widget(Paragraph::new(help).alignment(Alignment::Center), help_row);
    }

    app.hits.extend(clicks);
    if let Some(rect) = cover_rect {
        render_cover(f, app, rect);
    }
}
