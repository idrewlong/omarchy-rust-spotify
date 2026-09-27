//! "compact": everything in a small floating window. A small cover, then
//! the track and a progress line; no help row.

use super::*;

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let p = app.settings.palette;
    let base = Style::new().fg(p.fg).bg(p.bg);
    let area = f.area();
    f.render_widget(Block::new().style(base), area);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(p.muted))
        .style(base);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let s = app.state.clone();
    let mut clicks: Vec<(Rect, Hit)> = Vec::new();

    // Cover: a square on the left, at most 8 rows.
    let mut cover_rect = None;
    let mut text_area = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    if s.track.is_some() && app.cover.is_some() && app.settings.layout.cover != CoverPlacement::None
    {
        let font = app.picker.font_size();
        let rows = inner.height.saturating_sub(2).min(8);
        let cols = (rows as u32 * font.height.max(1) as u32 / font.width.max(1) as u32) as u16;
        if rows >= 3 && inner.width >= cols + 20 {
            let r = Rect {
                x: inner.x + 1,
                y: inner.y + (inner.height - rows) / 2,
                width: cols,
                height: rows,
            };
            cover_rect = Some(r);
            text_area = Rect {
                x: r.right() + 2,
                y: inner.y,
                width: inner.right().saturating_sub(r.right() + 3),
                height: inner.height,
            };
        }
    }

    let mut lines: Vec<Line> = Vec::new();
    if let Some(b) = banner(app) {
        lines.push(Line::styled(
            b,
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ));
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
        }
        None => lines.push(Line::styled("Nothing playing", Style::new().fg(p.muted))),
    }
    let text_h = lines.len() as u16;
    let [_, text_rect, bar_rect, status_rect, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(text_h),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(text_area);
    f.render_widget(Paragraph::new(lines), text_rect);

    if let Some(t) = &s.track {
        let pos = s.position_now_ms();
        let ratio = if t.duration_ms > 0 {
            pos as f64 / t.duration_ms as f64
        } else {
            0.0
        };
        f.render_widget(
            Paragraph::new(progress_line(
                bar_rect.width,
                ratio,
                app.settings.layout.progress,
                &p,
            )),
            bar_rect,
        );
        clicks.push((bar_rect, Hit::Seek));
        let icon = match s.status {
            Status::Playing => "▶",
            Status::Paused => "⏸",
            _ => "■",
        };
        let status = format!("{icon} {} / {}", fmt_ms(pos), fmt_ms(t.duration_ms));
        f.render_widget(
            Paragraph::new(Line::styled(status, Style::new().fg(p.muted))),
            status_rect,
        );
    }

    app.hits.extend(clicks);
    if let Some(r) = cover_rect {
        render_cover(f, app, r);
    }
}
