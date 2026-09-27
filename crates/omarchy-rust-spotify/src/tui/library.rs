//! "library": the default view. Find music and play it: a sidebar (Search,
//! Liked Songs, your playlists), the open list (a playlist, Liked Songs, an
//! album, an artist, search results), and a now-playing bar.
//!
//! Lists come from the daemon's library requests; a track plays within the
//! list it's in, so next/previous follow that playlist or album.

use std::collections::HashMap;

use omarchy_rust_spotify_proto::{Item, ItemKind, PlaylistOrder, Request, Section};

use super::*;

/// What the event loop should send for us.
pub(super) enum Out {
    Cmd(Command),
    Req(u64, Request),
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Sidebar,
    List,
}

/// Which view a response belongs to; stale ones (the user moved on) are
/// dropped.
#[derive(Clone, Copy)]
enum Target {
    Sidebar,
    View(u64),
    More(u64),
}

struct View {
    title: String,
    req: Request,
    /// What a track plays within ("liked", a playlist/album/artist URI), or
    /// None to play the track alone (search results).
    context: Option<String>,
    sections: Vec<Section>,
    loading: bool,
    loading_more: bool,
    error: Option<String>,
    sel: usize,
    scroll: usize,
    id: u64,
}

pub(super) struct Browser {
    focus: Focus,
    playlists: Vec<Item>,
    sidebar_error: Option<String>,
    sidebar_sel: usize,
    sidebar_scroll: usize,
    view: Option<View>,
    history: Vec<View>,
    searching: bool,
    query: String,
    pending: HashMap<u64, Target>,
    next_id: u64,
    started: bool,
    order: PlaylistOrder,
    /// Last frame's clickable rows: (rect, row index) for sidebar and list.
    sidebar_rows: Vec<(Rect, usize)>,
    list_rows: Vec<(Rect, usize)>,
    list_rect: Rect,
    sidebar_rect: Rect,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            focus: Focus::Sidebar,
            playlists: Vec::new(),
            sidebar_error: None,
            sidebar_sel: 1,
            sidebar_scroll: 0,
            view: None,
            history: Vec::new(),
            searching: false,
            query: String::new(),
            pending: HashMap::new(),
            next_id: 1000,
            started: false,
            order: PlaylistOrder::default(),
            sidebar_rows: Vec::new(),
            list_rows: Vec::new(),
            list_rect: Rect::default(),
            sidebar_rect: Rect::default(),
        }
    }
}

// Sidebar entries: 0 = Search, 1 = Liked Songs, 2 = "Playlists" heading,
// 3.. = playlists.
const SIDEBAR_FIXED: usize = 3;

enum Row<'a> {
    Header(&'a Section),
    Item(&'a Item),
}

fn rows(sections: &[Section]) -> Vec<Row<'_>> {
    let headers = sections.len() > 1;
    let mut out = Vec::new();
    for s in sections {
        if headers {
            out.push(Row::Header(s));
        }
        out.extend(s.items.iter().map(Row::Item));
    }
    out
}

impl Browser {
    fn id(&mut self, target: Target) -> u64 {
        self.next_id += 1;
        self.pending.insert(self.next_id, target);
        self.next_id
    }

    /// Ask for the playlists again (order changes as things are played).
    pub(super) fn refresh_playlists(&mut self, order: PlaylistOrder, out: &mut Vec<Out>) {
        self.order = order;
        let id = self.id(Target::Sidebar);
        out.push(Out::Req(id, Request::Playlists { order }));
    }

    /// First requests once connected: playlists, and Liked Songs open.
    pub(super) fn start(&mut self, order: PlaylistOrder, out: &mut Vec<Out>) {
        if self.started {
            return;
        }
        self.started = true;
        self.refresh_playlists(order, out);
        self.open(
            "Liked Songs".into(),
            Request::Tracks {
                of: "liked".into(),
                offset: 0,
            },
            Some("liked".into()),
            false,
            out,
        );
    }

    /// Open a list, remembering the current one for Back.
    fn open(
        &mut self,
        title: String,
        req: Request,
        context: Option<String>,
        push: bool,
        out: &mut Vec<Out>,
    ) {
        self.next_id += 1;
        let id = self.next_id;
        self.pending.insert(id, Target::View(id));
        if let Some(old) = self.view.take()
            && push
        {
            self.history.push(old);
        }
        self.view = Some(View {
            title,
            req: req.clone(),
            context,
            sections: Vec::new(),
            loading: true,
            loading_more: false,
            error: None,
            sel: 0,
            scroll: 0,
            id,
        });
        out.push(Out::Req(id, req));
    }

    pub(super) fn on_response(&mut self, msg: &ServerMsg) -> bool {
        let (id, result) = match msg {
            ServerMsg::Res { id, sections } => (*id, Ok(sections.clone())),
            ServerMsg::Err { id, message, .. } => (*id, Err(message.clone())),
            _ => return false,
        };
        let Some(target) = self.pending.remove(&id) else {
            return false;
        };
        match target {
            Target::Sidebar => match result {
                Ok(sections) => {
                    self.playlists = sections.into_iter().flat_map(|s| s.items).collect();
                    self.sidebar_error = None;
                }
                Err(e) => self.sidebar_error = Some(e),
            },
            Target::View(vid) => {
                if let Some(v) = self.view.as_mut().filter(|v| v.id == vid) {
                    v.loading = false;
                    match result {
                        Ok(sections) => {
                            if let Some(t) = sections
                                .first()
                                .map(|s| s.title.clone())
                                .filter(|t| !t.is_empty())
                                && sections.len() == 1
                            {
                                v.title = t;
                            }
                            v.sections = sections;
                            v.sel = first_item(v);
                        }
                        Err(e) => v.error = Some(e),
                    }
                }
            }
            Target::More(vid) => {
                if let Some(v) = self.view.as_mut().filter(|v| v.id == vid) {
                    v.loading_more = false;
                    if let (Ok(mut more), Some(sec)) = (result, v.sections.first_mut())
                        && let Some(m) = more.pop()
                    {
                        sec.items.extend(m.items);
                    }
                }
            }
        }
        true
    }

    /// Load the next page when the selection nears the end of what's loaded.
    fn maybe_more(&mut self, out: &mut Vec<Out>) {
        let Some(v) = self.view.as_ref() else { return };
        let Request::Tracks { of, .. } = &v.req else {
            return;
        };
        let Some(sec) = v.sections.first() else {
            return;
        };
        let loaded = sec.offset as usize + sec.items.len();
        if v.loading_more || loaded >= sec.total as usize || v.sel + 15 < sec.items.len() {
            return;
        }
        let (of, vid) = (of.clone(), v.id);
        let id = self.id(Target::More(vid));
        if let Some(v) = self.view.as_mut() {
            v.loading_more = true;
        }
        out.push(Out::Req(
            id,
            Request::Tracks {
                of,
                offset: loaded as u32,
            },
        ));
    }

    /// Opens the search field, as `/` does.
    pub(super) fn start_search(&mut self) {
        self.searching = true;
        self.query.clear();
    }

    fn sidebar_len(&self) -> usize {
        SIDEBAR_FIXED + self.playlists.len()
    }

    fn activate_sidebar(&mut self, out: &mut Vec<Out>) {
        match self.sidebar_sel {
            0 => {
                self.searching = true;
                self.query.clear();
            }
            1 => {
                self.history.clear();
                self.open(
                    "Liked Songs".into(),
                    Request::Tracks {
                        of: "liked".into(),
                        offset: 0,
                    },
                    Some("liked".into()),
                    false,
                    out,
                );
                self.focus = Focus::List;
            }
            i if i >= SIDEBAR_FIXED => {
                if let Some(p) = self.playlists.get(i - SIDEBAR_FIXED).cloned() {
                    self.history.clear();
                    self.open(
                        p.name.clone(),
                        Request::Tracks {
                            of: p.uri.clone(),
                            offset: 0,
                        },
                        Some(p.uri),
                        false,
                        out,
                    );
                    self.focus = Focus::List;
                }
            }
            _ => {}
        }
    }

    fn activate_row(&mut self, out: &mut Vec<Out>) {
        let Some(v) = self.view.as_ref() else { return };
        let rows = rows(&v.sections);
        let Some(Row::Item(item)) = rows.get(v.sel) else {
            return;
        };
        let item = (*item).clone();
        match item.kind {
            ItemKind::Track => {
                // Tracks in an artist's "Popular" play in the artist
                // context; album lists under an artist don't have tracks.
                let context = v.context.clone();
                // "Recents": what you just played goes to the top now,
                // without waiting for the daemon's next answer.
                if let Some(ctx) = &context
                    && self.order == PlaylistOrder::Recent
                    && let Some(i) = self.playlists.iter().position(|p| &p.uri == ctx)
                {
                    let p = self.playlists.remove(i);
                    self.playlists.insert(0, p);
                }
                let cmd = match context {
                    Some(ctx) => Command::PlayIn {
                        context: ctx,
                        track: Some(item.uri),
                    },
                    None => Command::PlayIn {
                        context: item.uri,
                        track: None,
                    },
                };
                out.push(Out::Cmd(cmd));
            }
            ItemKind::Album => self.open(
                item.name.clone(),
                Request::Tracks {
                    of: item.uri.clone(),
                    offset: 0,
                },
                Some(item.uri),
                true,
                out,
            ),
            ItemKind::Playlist => self.open(
                item.name.clone(),
                Request::Tracks {
                    of: item.uri.clone(),
                    offset: 0,
                },
                Some(item.uri),
                true,
                out,
            ),
            ItemKind::Artist => self.open(
                item.name.clone(),
                Request::Artist {
                    uri: item.uri.clone(),
                },
                Some(item.uri),
                true,
                out,
            ),
        }
    }

    fn back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.view = Some(prev);
        } else {
            self.focus = Focus::Sidebar;
        }
    }

    /// Move the selection `delta` items, skipping section headers.
    fn move_list(&mut self, delta: i64, out: &mut Vec<Out>) {
        let Some(v) = self.view.as_mut() else { return };
        let rows = rows(&v.sections);
        let step = delta.signum();
        for _ in 0..delta.abs() {
            let mut j = v.sel as i64 + step;
            while (0..rows.len() as i64).contains(&j) && matches!(rows[j as usize], Row::Header(_))
            {
                j += step;
            }
            if !(0..rows.len() as i64).contains(&j) {
                break;
            }
            v.sel = j as usize;
        }
        self.maybe_more(out);
    }

    fn move_sidebar(&mut self, delta: i64) {
        let len = self.sidebar_len() as i64;
        let mut i = (self.sidebar_sel as i64 + delta).clamp(0, len - 1);
        if i == 2 {
            i += delta.signum();
        }
        self.sidebar_sel = i.clamp(0, len - 1) as usize;
    }

    /// Keys the library handles itself; `None` means "not mine", so global
    /// keys (play/pause, seek, volume, skins) apply.
    pub(super) fn on_key(
        &mut self,
        code: KeyCode,
        mods: KeyModifiers,
        out: &mut Vec<Out>,
    ) -> Option<()> {
        if self.searching {
            match code {
                KeyCode::Esc => self.searching = false,
                KeyCode::Enter => {
                    self.searching = false;
                    let q = self.query.trim().to_string();
                    if !q.is_empty() {
                        self.history.clear();
                        self.open(
                            format!("Search: {q}"),
                            Request::Search { q },
                            None,
                            false,
                            out,
                        );
                        self.focus = Focus::List;
                    }
                }
                KeyCode::Backspace => {
                    self.query.pop();
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) => self.query.push(c),
                _ => {}
            }
            return Some(());
        }
        match (code, self.focus) {
            (KeyCode::Char('/'), _) => {
                self.searching = true;
                self.query.clear();
            }
            (KeyCode::Tab | KeyCode::BackTab, _) => {
                self.focus = if self.focus == Focus::List {
                    Focus::Sidebar
                } else {
                    Focus::List
                };
            }
            (KeyCode::Char('h'), _) => self.focus = Focus::Sidebar,
            (KeyCode::Char('l'), _) => self.focus = Focus::List,
            (KeyCode::Up | KeyCode::Char('k'), Focus::Sidebar) => self.move_sidebar(-1),
            (KeyCode::Down | KeyCode::Char('j'), Focus::Sidebar) => self.move_sidebar(1),
            (KeyCode::Enter, Focus::Sidebar) => self.activate_sidebar(out),
            (KeyCode::Up | KeyCode::Char('k'), Focus::List) => self.move_list(-1, out),
            (KeyCode::Down | KeyCode::Char('j'), Focus::List) => self.move_list(1, out),
            (KeyCode::PageUp, Focus::List) => self.move_list(-15, out),
            (KeyCode::PageDown, Focus::List) => self.move_list(15, out),
            (KeyCode::Enter, Focus::List) => self.activate_row(out),
            (KeyCode::Esc | KeyCode::Backspace, Focus::List) => self.back(),
            _ => return None,
        }
        Some(())
    }

    pub(super) fn on_mouse(
        &mut self,
        kind: MouseEventKind,
        col: u16,
        row: u16,
        out: &mut Vec<Out>,
    ) -> bool {
        let inside = |r: Rect| col >= r.x && col < r.right() && row >= r.y && row < r.bottom();
        match kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let d = if kind == MouseEventKind::ScrollDown {
                    3
                } else {
                    -3
                };
                if inside(self.list_rect) {
                    self.focus = Focus::List;
                    self.move_list(d, out);
                } else if inside(self.sidebar_rect) {
                    self.focus = Focus::Sidebar;
                    self.move_sidebar(d);
                } else {
                    return false;
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(&(_, i)) = self.sidebar_rows.iter().find(|(r, _)| inside(*r)) {
                    self.focus = Focus::Sidebar;
                    // Click selects; clicking the selected entry opens it.
                    if self.sidebar_sel == i {
                        self.activate_sidebar(out);
                    } else if i != 2 {
                        self.sidebar_sel = i;
                        self.activate_sidebar(out);
                    }
                    return true;
                }
                if let Some(&(_, i)) = self.list_rows.iter().find(|(r, _)| inside(*r)) {
                    self.focus = Focus::List;
                    let again = self.view.as_ref().is_some_and(|v| v.sel == i);
                    if let Some(v) = self.view.as_mut() {
                        v.sel = i;
                    }
                    if again {
                        self.activate_row(out);
                    }
                    self.maybe_more(out);
                    return true;
                }
                // Anything else (the progress bar) is the shared click
                // regions' job.
                false
            }
            _ => false,
        }
    }
}

/// The longest key help that fits: never cut off mid-word.
fn help_for(width: u16) -> &'static str {
    const TIERS: [&str; 3] = [
        "/ search · tab panes · ↑↓ move · enter play/open · esc back · space pause · n/p next/prev · s shuffle · r repeat · +/- volume · t skin · q quit",
        "/ search · tab panes · enter play/open · esc back · space pause · n/p · s/r · +/- · t skin · q quit",
        "/ search · enter play · esc back · space pause · q quit",
    ];
    TIERS
        .iter()
        .copied()
        .find(|t| t.chars().count() <= width as usize)
        .unwrap_or("q quit")
}

fn first_item(v: &View) -> usize {
    rows(&v.sections)
        .iter()
        .position(|r| matches!(r, Row::Item(_)))
        .unwrap_or(0)
}

/// Keep `sel` visible in a window of `height` rows.
fn scrolled(sel: usize, scroll: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    if sel < scroll {
        sel
    } else if sel >= scroll + height {
        sel + 1 - height
    } else {
        scroll
    }
}

fn pad(s: &str, w: usize) -> String {
    let mut out: String = s.chars().take(w).collect();
    let n = out.chars().count();
    if n < w {
        out.push_str(&" ".repeat(w - n));
    } else if s.chars().count() > w && w > 1 {
        out.pop();
        out.push('…');
    }
    out
}

/// How a skin wants the sidebar and lists drawn.
#[derive(Clone)]
pub(super) struct ListStyle {
    pub bg: Color,
    pub fg: Color,
    /// Subtitles, headings, placeholder text.
    pub dim: Color,
    /// The row that's playing now.
    pub playing: Color,
    /// Selection in the focused pane.
    pub sel_fg: Color,
    pub sel_bg: Color,
    /// Selection in the other pane.
    pub sel_unfocused: Style,
    pub header: Style,
    /// Alternate rows on this background (iTunes' stripes).
    pub stripe: Option<Color>,
    /// "12. " before each track (Winamp's playlist).
    pub numbered: bool,
    pub playing_mark: &'static str,
}

impl ListStyle {
    /// The default look, from the Omarchy theme.
    pub fn themed(p: &Palette) -> Self {
        Self {
            bg: p.bg,
            fg: p.fg,
            dim: p.muted,
            playing: p.accent,
            sel_fg: p.bg,
            sel_bg: p.accent,
            sel_unfocused: Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
            header: Style::new().fg(p.muted).add_modifier(Modifier::BOLD),
            stripe: None,
            numbered: false,
            playing_mark: "▶ ",
        }
    }
}

impl Browser {
    pub(super) fn list_title(&self) -> String {
        match (&self.view, self.searching) {
            (_, true) => format!(" Search: {}▏", self.query),
            (Some(v), _) => {
                let total = v
                    .sections
                    .first()
                    .filter(|_| v.sections.len() == 1)
                    .map(|s| format!(" · {}", s.total))
                    .unwrap_or_default();
                format!(" {}{} ", v.title, total)
            }
            (None, _) => " ".into(),
        }
    }

    /// A browser already filled in: the demo's library (no requests).
    pub(super) fn demo(playlists: Vec<Item>, tracks: Vec<Item>, liked_total: u32) -> Self {
        let n = tracks.len();
        Browser {
            started: true,
            focus: Focus::List,
            playlists,
            sidebar_sel: 1,
            view: Some(View {
                title: "Liked Songs".into(),
                req: Request::Tracks {
                    of: "liked".into(),
                    offset: 0,
                },
                context: Some("liked".into()),
                sections: vec![Section {
                    title: String::new(),
                    items: tracks,
                    total: liked_total.max(n as u32),
                    offset: 0,
                }],
                loading: false,
                loading_more: false,
                error: None,
                sel: 2,
                scroll: 0,
                id: 0,
            }),
            ..Default::default()
        }
    }

    /// The open list's own title (no count) and what it plays within,
    /// for skins with a big play button (Spotify's).
    pub(super) fn open_list(&self) -> Option<(String, Option<String>)> {
        let v = self.view.as_ref()?;
        Some((v.title.clone(), v.context.clone()))
    }

    pub(super) fn sidebar_focused(&self) -> bool {
        self.focus == Focus::Sidebar && !self.searching
    }

    pub(super) fn searching(&self) -> bool {
        self.searching
    }

    pub(super) fn focus_sidebar(&mut self) {
        self.focus = Focus::Sidebar;
    }

    pub(super) fn list_focused(&self) -> bool {
        self.focus == Focus::List || self.searching
    }

    /// The row count of the open list and the index of the playing row, for
    /// status lines ("12 of 180").
    pub(super) fn position_of(&self, uri: Option<&str>) -> (usize, Option<usize>) {
        let Some(v) = &self.view else {
            return (0, None);
        };
        let items: Vec<&Item> = v.sections.iter().flat_map(|s| &s.items).collect();
        let total = v
            .sections
            .first()
            .filter(|_| v.sections.len() == 1)
            .map(|s| s.total as usize)
            .unwrap_or(items.len());
        (
            total,
            uri.and_then(|u| items.iter().position(|i| i.uri == u)),
        )
    }
}

fn row_style(
    st: &ListStyle,
    selected: bool,
    focused: bool,
    playing: bool,
    stripe_row: bool,
) -> Style {
    let bg = match st.stripe {
        Some(c) if stripe_row => c,
        _ => st.bg,
    };
    let base = Style::new()
        .fg(if playing { st.playing } else { st.fg })
        .bg(bg);
    match (selected, focused) {
        (true, true) => base
            .fg(st.sel_fg)
            .bg(st.sel_bg)
            .add_modifier(Modifier::BOLD),
        (true, false) => base.patch(st.sel_unfocused),
        _ => base,
    }
}

/// The sidebar's rows (Search, Liked Songs, playlists) inside `rect`.
pub(super) fn draw_sidebar(f: &mut Frame, b: &mut Browser, rect: Rect, st: &ListStyle) {
    b.sidebar_rect = rect;
    b.sidebar_rows.clear();
    let len = b.sidebar_len();
    let h = rect.height as usize;
    b.sidebar_scroll = scrolled(b.sidebar_sel, b.sidebar_scroll, h);
    let focused = b.focus == Focus::Sidebar && !b.searching;
    for (row, i) in (b.sidebar_scroll..len).enumerate().take(h) {
        let r = Rect {
            x: rect.x,
            y: rect.y + row as u16,
            width: rect.width,
            height: 1,
        };
        let (label, heading) = match i {
            0 => ("⌕ Search".to_string(), false),
            1 => ("♥ Liked Songs".to_string(), false),
            2 => {
                let t = match (&b.sidebar_error, b.playlists.is_empty()) {
                    (Some(_), _) => "Playlists (unavailable)",
                    (None, true) => "Playlists (loading…)",
                    _ => "Playlists",
                };
                (t.to_string(), true)
            }
            _ => (b.playlists[i - SIDEBAR_FIXED].name.clone(), false),
        };
        let style = if heading {
            st.header.bg(st.bg)
        } else {
            row_style(st, i == b.sidebar_sel, focused, false, false)
        };
        let w = (rect.width as usize).saturating_sub(1);
        // Search lines up with the rows below; its key sits at the far edge.
        let text = if i == 0 && w > 12 {
            format!(" {}/ ", pad(&label, w - 2))
        } else {
            format!(" {}", pad(&label, w))
        };
        f.render_widget(Paragraph::new(Line::styled(text, style)), r);
        b.sidebar_rows.push((r, i));
    }
}

/// The open list (or search prompt, loading, error) inside `rect`.
/// Widths of a song row's columns, for skins that draw column headers.
pub(super) struct Columns {
    pub num_w: usize,
    pub mark_w: usize,
    pub name_w: usize,
    pub sub_w: usize,
    pub time_w: usize,
}

pub(super) fn columns(width: u16, st: &ListStyle) -> Columns {
    let num_w = if st.numbered { 5 } else { 0 };
    let mark_w = st.playing_mark.chars().count();
    let time_w = 6;
    let avail = (width as usize).saturating_sub(num_w + mark_w + time_w + 2);
    let sub_w = avail * 2 / 5;
    Columns {
        num_w,
        mark_w,
        name_w: avail.saturating_sub(sub_w + 1),
        sub_w,
        time_w,
    }
}

pub(super) fn draw_list(
    f: &mut Frame,
    b: &mut Browser,
    rect: Rect,
    current: Option<&str>,
    st: &ListStyle,
) {
    b.list_rect = rect;
    b.list_rows.clear();
    let msg = |f: &mut Frame, text: &str, color: Color| {
        f.render_widget(
            Paragraph::new(Line::styled(
                text.to_string(),
                Style::new().fg(color).bg(st.bg),
            ))
            .alignment(Alignment::Center),
            Rect {
                y: rect.y + rect.height / 2,
                height: 1,
                ..rect
            },
        );
    };
    if b.searching {
        msg(
            f,
            "Type to search songs, artists, albums and playlists · Enter to search · Esc to cancel",
            st.dim,
        );
        return;
    }
    let focused = b.focus == Focus::List;
    let Some(v) = b.view.as_mut() else { return };
    if let Some(e) = &v.error {
        return msg(f, e, st.playing);
    }
    if v.loading {
        return msg(f, "Loading…", st.dim);
    }
    let rows = rows(&v.sections);
    if rows.is_empty() {
        return msg(f, "Nothing here", st.dim);
    }
    let h = rect.height as usize;
    v.scroll = scrolled(v.sel, v.scroll, h);
    let Columns {
        num_w: _,
        mark_w,
        name_w,
        sub_w,
        time_w,
    } = columns(rect.width, st);
    let w = rect.width as usize;
    for (row, i) in (v.scroll..rows.len()).enumerate().take(h) {
        let r = Rect {
            x: rect.x,
            y: rect.y + row as u16,
            width: rect.width,
            height: 1,
        };
        match &rows[i] {
            Row::Header(s) => {
                let t = format!(" {:<w$}", s.title, w = w.saturating_sub(1));
                f.render_widget(Paragraph::new(Line::styled(t, st.header.bg(st.bg))), r);
            }
            Row::Item(it) => {
                let playing = current == Some(it.uri.as_str());
                let mark = if playing {
                    st.playing_mark.to_string()
                } else {
                    " ".repeat(mark_w)
                };
                let num = if st.numbered {
                    // Track number within the whole list, as Winamp's
                    // playlist editor showed it (headers don't count).
                    let before = rows[..i]
                        .iter()
                        .filter(|r| matches!(r, Row::Item(_)))
                        .count();
                    format!("{:>3}. ", before + 1)
                } else {
                    String::new()
                };
                let dur = it.duration_ms.map(fmt_ms).unwrap_or_default();
                let line = format!(
                    "{num}{mark}{} {} {:>time_w$} ",
                    pad(&it.name, name_w),
                    pad(&it.subtitle, sub_w),
                    dur
                );
                let style = row_style(st, i == v.sel, focused, playing, (v.scroll + row) % 2 == 1);
                f.render_widget(Paragraph::new(Line::styled(line, style)), r);
                b.list_rows.push((r, i));
            }
        }
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let p = app.settings.palette;
    let base = Style::new().fg(p.fg).bg(p.bg);
    let area = f.area();
    f.render_widget(Block::new().style(base), area);
    if area.width < 50 || area.height < 14 {
        return classic::draw(f, app);
    }

    let [main, bar, help] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(6),
        Constraint::Length(1),
    ])
    .areas(area);
    let side_w = (area.width / 4).clamp(22, 34);
    let [side, list] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Fill(1)]).areas(main);

    let focused = |on: bool| Style::new().fg(if on { p.accent } else { p.muted });
    let b = &mut app.browser;

    // ---- sidebar
    let side_block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused(b.focus == Focus::Sidebar && !b.searching))
        .title(Span::styled(
            " Library ",
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ))
        .style(base);
    let side_in = side_block.inner(side);
    f.render_widget(side_block, side);
    let st = ListStyle::themed(&p);
    draw_sidebar(f, b, side_in, &st);

    // ---- list
    let title = b.list_title();
    let list_block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused(b.focus == Focus::List || b.searching))
        .title(Span::styled(
            title,
            Style::new().fg(p.accent).add_modifier(Modifier::BOLD),
        ))
        .style(base);
    let list_in = list_block.inner(list);
    f.render_widget(list_block, list);
    let current = app.state.track.as_ref().map(|t| t.uri.clone());
    draw_list(f, b, list_in, current.as_deref(), &st);

    // ---- now-playing bar
    let bar_block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(p.muted))
        .style(base);
    let bar_in = bar_block.inner(bar);
    f.render_widget(bar_block, bar);
    let s = app.state.clone();
    let font = app.picker.font_size();
    let cover_h = bar_in.height;
    let cover_w = (cover_h as u32 * font.height.max(1) as u32 / font.width.max(1) as u32) as u16;
    let has_cover = app.cover.is_some() && s.track.is_some();
    let text_x = if has_cover {
        bar_in.x + cover_w + 2
    } else {
        bar_in.x + 1
    };
    let text_w = bar_in.right().saturating_sub(text_x + 1);
    let line = |f: &mut Frame, y: u16, l: Line| {
        f.render_widget(
            Paragraph::new(l),
            Rect {
                x: text_x,
                y,
                width: text_w,
                height: 1,
            },
        );
    };
    if let Some(b) = banner(app) {
        line(
            f,
            bar_in.y,
            Line::styled(b, Style::new().fg(p.accent).add_modifier(Modifier::BOLD)),
        );
    } else if let Some(t) = &s.track {
        line(
            f,
            bar_in.y,
            Line::styled(
                t.name.clone(),
                Style::new().fg(p.fg).add_modifier(Modifier::BOLD),
            ),
        );
        line(
            f,
            bar_in.y + 1,
            Line::styled(t.artists.join(", "), Style::new().fg(p.accent)),
        );
    } else {
        line(
            f,
            bar_in.y,
            Line::styled(
                "Nothing playing: pick something above",
                Style::new().fg(p.muted),
            ),
        );
    }
    let mut progress = None;
    if let Some(t) = &s.track {
        let pos = s.position_now_ms();
        let ratio = if t.duration_ms > 0 {
            pos as f64 / t.duration_ms as f64
        } else {
            0.0
        };
        let times = format!(" {} / {}", fmt_ms(pos), fmt_ms(t.duration_ms));
        let bar_w = text_w.saturating_sub(times.len() as u16 + 1);
        let r = Rect {
            x: text_x,
            y: bar_in.y + 2,
            width: bar_w,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(progress_line(
                bar_w,
                ratio,
                app.settings.layout.progress,
                &p,
            )),
            r,
        );
        f.render_widget(
            Paragraph::new(Line::styled(times, Style::new().fg(p.muted))),
            Rect {
                x: r.right() + 1,
                y: r.y,
                width: text_w - bar_w,
                height: 1,
            },
        );
        progress = Some(r);
        let play = if s.status == Status::Playing {
            "▶ playing"
        } else {
            "⏸ paused"
        };
        let on = |b: bool| if b { p.accent } else { p.muted };
        line(
            f,
            bar_in.y + 3,
            Line::from(vec![
                Span::styled(play, Style::new().fg(p.fg)),
                Span::styled(
                    match s.track.as_ref().and_then(|t| t.liked) {
                        Some(true) => "   ♥ liked",
                        Some(false) => "   ♡ like (f)",
                        None => "",
                    },
                    Style::new().fg(on(s.track.as_ref().and_then(|t| t.liked) == Some(true))),
                ),
                Span::styled("   shuffle", Style::new().fg(on(s.shuffle))),
                Span::styled(
                    match s.repeat {
                        Repeat::Off => "   repeat off",
                        Repeat::Context => "   repeat all",
                        Repeat::Track => "   repeat one",
                    },
                    Style::new().fg(on(s.repeat != Repeat::Off)),
                ),
                Span::styled(
                    format!("   vol {}%   {}", s.volume, s.device_name),
                    Style::new().fg(p.muted),
                ),
            ]),
        );
    }
    if let Some(r) = progress {
        app.hits.push((r, Hit::Seek));
    }

    let help_text = match &app.settings.problem {
        Some(problem) => Line::styled(problem.clone(), Style::new().fg(p.accent)),
        None => Line::styled(help_for(help.width), Style::new().fg(p.muted)),
    };
    f.render_widget(Paragraph::new(help_text).alignment(Alignment::Center), help);

    if has_cover {
        render_cover(
            f,
            app,
            Rect {
                x: bar_in.x,
                y: bar_in.y,
                width: cover_w,
                height: cover_h,
            },
        );
    }
}

#[cfg(test)]
impl Browser {
    /// A populated browser: playlists and a two-section view (with headers).
    pub(super) fn sample() -> Self {
        let item = |kind, name: &str| Item {
            kind,
            uri: format!("spotify:track:{name}"),
            name: format!("{name} with a rather long name that won't fit"),
            subtitle: "Some Artist, Another Artist".into(),
            duration_ms: Some(215_000),
            image: None,
        };
        let mut b = Browser {
            started: true,
            focus: Focus::List,
            ..Default::default()
        };
        b.playlists = (0..40)
            .map(|i| item(ItemKind::Playlist, &format!("P{i}")))
            .collect();
        b.view = Some(View {
            title: "Search: x".into(),
            req: Request::Search { q: "x".into() },
            context: None,
            sections: vec![
                Section {
                    title: "Songs".into(),
                    items: (0..30)
                        .map(|i| item(ItemKind::Track, &format!("T{i}")))
                        .collect(),
                    total: 30,
                    offset: 0,
                },
                Section {
                    title: "Albums".into(),
                    items: (0..5)
                        .map(|i| item(ItemKind::Album, &format!("A{i}")))
                        .collect(),
                    total: 5,
                    offset: 0,
                },
            ],
            loading: false,
            loading_more: false,
            error: None,
            sel: 25,
            scroll: 0,
            id: 1,
        });
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(uri: &str) -> Item {
        Item {
            kind: ItemKind::Track,
            uri: uri.into(),
            name: uri.into(),
            subtitle: String::new(),
            duration_ms: Some(1000),
            image: None,
        }
    }

    fn reqs(out: &[Out]) -> Vec<(u64, Request)> {
        out.iter()
            .filter_map(|o| {
                if let Out::Req(id, r) = o {
                    Some((*id, r.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    fn cmds(out: &[Out]) -> Vec<Command> {
        out.iter()
            .filter_map(|o| {
                if let Out::Cmd(c) = o {
                    Some(c.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn key(b: &mut Browser, code: KeyCode) -> Vec<Out> {
        let mut out = Vec::new();
        b.on_key(code, KeyModifiers::NONE, &mut out);
        out
    }

    fn answer(b: &mut Browser, id: u64, sections: Vec<Section>) {
        assert!(b.on_response(&ServerMsg::Res { id, sections }));
    }

    #[test]
    fn start_loads_playlists_and_opens_liked() {
        let mut b = Browser::default();
        let mut out = Vec::new();
        b.start(PlaylistOrder::Recent, &mut out);
        let r = reqs(&out);
        assert!(matches!(
            r[0].1,
            Request::Playlists {
                order: PlaylistOrder::Recent
            }
        ));
        assert_eq!(
            r[1].1,
            Request::Tracks {
                of: "liked".into(),
                offset: 0
            }
        );
        // Starting twice does nothing.
        let mut again = Vec::new();
        b.start(PlaylistOrder::Recent, &mut again);
        assert!(again.is_empty());
    }

    #[test]
    fn enter_plays_a_liked_track_in_liked() {
        let mut b = Browser::default();
        let mut out = Vec::new();
        b.start(PlaylistOrder::Recent, &mut out);
        let view_id = reqs(&out)[1].0;
        answer(
            &mut b,
            view_id,
            vec![Section {
                title: "Liked Songs".into(),
                items: vec![track("a"), track("b")],
                total: 2,
                offset: 0,
            }],
        );
        b.focus = Focus::List;
        key(&mut b, KeyCode::Down);
        let out = key(&mut b, KeyCode::Enter);
        assert_eq!(
            cmds(&out),
            vec![Command::PlayIn {
                context: "liked".into(),
                track: Some("b".into())
            }]
        );
    }

    #[test]
    fn search_then_open_album_then_back() {
        let mut b = Browser::default();
        key(&mut b, KeyCode::Char('/'));
        for c in "wil".chars() {
            key(&mut b, KeyCode::Char(c));
        }
        let out = key(&mut b, KeyCode::Enter);
        let (id, req) = reqs(&out).remove(0);
        assert_eq!(req, Request::Search { q: "wil".into() });
        let album = Item {
            kind: ItemKind::Album,
            uri: "spotify:album:x".into(),
            name: "X".into(),
            subtitle: String::new(),
            duration_ms: None,
            image: None,
        };
        answer(
            &mut b,
            id,
            vec![
                Section {
                    title: "Songs".into(),
                    items: vec![track("t1")],
                    total: 1,
                    offset: 0,
                },
                Section {
                    title: "Albums".into(),
                    items: vec![album],
                    total: 1,
                    offset: 0,
                },
            ],
        );
        // Starts on the first item (not the "Songs" header); a search
        // result plays on its own.
        let out = key(&mut b, KeyCode::Enter);
        assert_eq!(
            cmds(&out),
            vec![Command::PlayIn {
                context: "t1".into(),
                track: None
            }]
        );
        // Down skips the "Albums" header onto the album; Enter opens it.
        key(&mut b, KeyCode::Down);
        let out = key(&mut b, KeyCode::Enter);
        assert_eq!(
            reqs(&out)[0].1,
            Request::Tracks {
                of: "spotify:album:x".into(),
                offset: 0
            }
        );
        // Esc returns to the search results.
        key(&mut b, KeyCode::Esc);
        assert!(matches!(
            b.view.as_ref().unwrap().req,
            Request::Search { .. }
        ));
    }

    #[test]
    fn playing_from_a_playlist_moves_it_to_the_top() {
        let mut b = Browser::default();
        let mut out = Vec::new();
        b.start(PlaylistOrder::Recent, &mut out);
        let pl = |n: &str| Item {
            kind: ItemKind::Playlist,
            uri: format!("spotify:playlist:{n}"),
            name: n.into(),
            subtitle: String::new(),
            duration_ms: None,
            image: None,
        };
        b.playlists = vec![pl("a"), pl("b"), pl("c")];
        // Open "c" from the sidebar and play its first track.
        b.sidebar_sel = SIDEBAR_FIXED + 2;
        let out = key(&mut b, KeyCode::Enter);
        let id = reqs(&out)[0].0;
        answer(
            &mut b,
            id,
            vec![Section {
                title: "c".into(),
                items: vec![track("x")],
                total: 1,
                offset: 0,
            }],
        );
        let out = key(&mut b, KeyCode::Enter);
        assert_eq!(
            cmds(&out),
            vec![Command::PlayIn {
                context: "spotify:playlist:c".into(),
                track: Some("x".into())
            }]
        );
        assert_eq!(b.playlists[0].name, "c");
    }

    #[test]
    fn stale_responses_are_dropped() {
        let mut b = Browser::default();
        let mut out = Vec::new();
        b.start(PlaylistOrder::Recent, &mut out);
        let liked_id = reqs(&out)[1].0;
        // The user opens a search before Liked Songs arrives.
        key(&mut b, KeyCode::Char('/'));
        key(&mut b, KeyCode::Char('x'));
        key(&mut b, KeyCode::Enter);
        answer(
            &mut b,
            liked_id,
            vec![Section {
                title: "Liked Songs".into(),
                items: vec![track("a")],
                total: 1,
                offset: 0,
            }],
        );
        let v = b.view.as_ref().unwrap();
        assert!(matches!(v.req, Request::Search { .. }));
        assert!(
            v.sections.is_empty(),
            "the late Liked Songs answer must not land in the search view"
        );
    }

    #[test]
    fn scrolling_near_the_end_loads_the_next_page() {
        let mut b = Browser::default();
        let mut out = Vec::new();
        b.start(PlaylistOrder::Recent, &mut out);
        let id = reqs(&out)[1].0;
        let items = (0..50).map(|i| track(&format!("t{i}"))).collect();
        answer(
            &mut b,
            id,
            vec![Section {
                title: "Liked Songs".into(),
                items,
                total: 120,
                offset: 0,
            }],
        );
        b.focus = Focus::List;
        let mut out = Vec::new();
        b.move_list(40, &mut out);
        assert_eq!(
            reqs(&out).last().unwrap().1,
            Request::Tracks {
                of: "liked".into(),
                offset: 50
            }
        );
    }
}
