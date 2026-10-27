//! Shared helpers for screens.

use crate::app::{App, CoverState};
use crate::host::Host;
use oc_ui::{Align, Color, Icon, Rect, RectExt, Ui, Weight};

pub fn fmt_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Draws a cover thumbnail (requesting it when needed) or a placeholder.
#[allow(clippy::too_many_arguments)]
pub fn draw_cover(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, key: &str, track_id: &str, radius: f32, alpha: f32) {
    if let Some((tex, w, h)) = app.cover_texture(key) {
        ui.image_cover(r, tex, w, h, Color(1.0, 1.0, 1.0, alpha), radius);
        return;
    }
    if !app.covers.contains_key(key) && !track_id.is_empty() {
        app.request_cover(host, key, track_id);
    }
    let t = ui.theme().clone();
    ui.rect(r, Color(1.0, 1.0, 1.0, 0.08 * alpha), radius);
    let icon = if app.covers.get(key) == Some(&CoverState::Loading) { Icon::Music } else { Icon::Album };
    ui.icon(r, icon, (r.w * 0.42).min(48.0), t.text_faint.mul_alpha(alpha));
}

/// Page title with optional subtitle; returns the remaining area.
pub fn page_header(ui: &mut Ui, r: Rect, title: &str, subtitle: &str) -> Rect {
    let t = ui.theme().clone();
    ui.text(Rect::new(r.x, r.y, r.w, 34.0), title, t.font_title, Weight::Bold, t.text, Align::Left);
    let mut y = r.y + 36.0;
    if !subtitle.is_empty() {
        ui.text(Rect::new(r.x, y, r.w, 20.0), subtitle, 13.0, Weight::Regular, t.text_dim, Align::Left);
        y += 24.0;
    }
    Rect::new(r.x, y + 8.0, r.w, (r.bottom() - y - 8.0).max(0.0))
}

pub fn section_title(ui: &mut Ui, r: Rect, title: &str) {
    let t = ui.theme().clone();
    ui.text(r, title, 12.0, Weight::SemiBold, t.text_faint, Align::Left);
}

/// A form row: label on the left, control area returned on the right.
pub fn field_row(ui: &mut Ui, r: Rect, label: &str, help: &str) -> Rect {
    let t = ui.theme().clone();
    let label_w = (r.w * 0.42).clamp(120.0, 260.0);
    let (l, c) = r.split_left(label_w);
    if help.is_empty() {
        ui.text(l.pad(0.0, 0.0), label, 14.0, Weight::Regular, t.text, Align::Left);
    } else {
        ui.text(Rect::new(l.x, l.y + 2.0, l.w, l.h / 2.0), label, 14.0, Weight::Regular, t.text, Align::Left);
        ui.text(Rect::new(l.x, l.y + l.h / 2.0 - 2.0, l.w - 8.0, l.h / 2.0), help, 11.0, Weight::Regular, t.text_faint, Align::Left);
    }
    Rect::new(c.x, c.y + (c.h - 34.0) / 2.0, c.w, 34.0)
}

/// Empty state message in the middle of `r`.
pub fn empty_state(ui: &mut Ui, r: Rect, icon: Icon, title: &str, text: &str) {
    let t = ui.theme().clone();
    let c = r.centered(r.w.min(420.0), 160.0);
    ui.icon(Rect::new(c.x, c.y, c.w, 48.0), icon, 40.0, t.text_faint);
    ui.text(Rect::new(c.x, c.y + 56.0, c.w, 24.0), title, 16.0, Weight::SemiBold, t.text, Align::Center);
    let lines_r = Rect::new(c.x, c.y + 86.0, c.w, 60.0);
    let lines = ui.ctx.fonts.wrap(text, 13.0, Weight::Regular, lines_r.w);
    for (i, l) in lines.iter().enumerate() {
        ui.text(Rect::new(lines_r.x, lines_r.y + i as f32 * 18.0, lines_r.w, 18.0), l, 13.0, Weight::Regular, t.text_dim, Align::Center);
    }
}

/// Toolbar button sized to its label.
pub fn tool_button(ui: &mut Ui, x: &mut f32, y: f32, id: u64, label: &str, icon: Option<Icon>, primary: bool) -> bool {
    let w = ui.measure(label, 14.0, Weight::Medium) + if icon.is_some() { 52.0 } else { 28.0 };
    let r = Rect::new(*x, y, w, 34.0);
    *x += w + 8.0;
    ui.button(id, r, label, icon, primary).clicked
}

/// Toolbar button aligned from the right edge.
pub fn tool_button_right(ui: &mut Ui, right: &mut f32, y: f32, id: u64, label: &str, icon: Option<Icon>, primary: bool) -> bool {
    let w = if label.is_empty() { 34.0 } else { ui.measure(label, 14.0, Weight::Medium) + if icon.is_some() { 52.0 } else { 28.0 } };
    *right -= w;
    let r = Rect::new(*right, y, w, 34.0);
    *right -= 8.0;
    ui.button(id, r, label, icon, primary).clicked
}

/// Row background for list items (hover/selection/current).
pub fn row_background(ui: &mut Ui, r: Rect, hovered: bool, current: bool, index: usize) {
    let t = ui.theme().clone();
    if current {
        ui.rect(r, t.primary.with_alpha(0.22), 8.0);
    } else if hovered {
        ui.rect(r, t.surface_hover, 8.0);
    } else if index % 2 == 1 {
        ui.rect(r, Color(1.0, 1.0, 1.0, 0.02), 8.0);
    }
}

/// Secondary action: right click, or a long press on touch screens.
pub fn secondary(ui: &Ui, resp: &oc_ui::Response, r: Rect) -> bool {
    if resp.secondary_clicked {
        return true;
    }
    let inp = ui.input;
    if inp.touch && inp.down[0] && ui.hovered(r) && inp.drag_distance() < 10.0 && !ui.is_drag_scrolling() {
        let held = ui.time() - inp.press_time;
        let dt = inp.dt as f64;
        return held >= 0.55 && held - dt < 0.55;
    }
    false
}

/// Context menu actions for tracks.
pub const TRACK_MENU: [(&str, Icon); 6] = [
    ("Play", Icon::Play),
    ("Play next", Icon::Next),
    ("Add to queue", Icon::Queue),
    ("Add to playlist…", Icon::Playlist),
    ("Go to album", Icon::Album),
    ("Go to artist", Icon::Artist),
];

pub fn track_menu_items(app: &App) -> Vec<oc_ui::MenuItem> {
    TRACK_MENU.iter().map(|(l, i)| oc_ui::MenuItem::new(app.tr(l)).icon(*i)).collect()
}

/// Executes a track context menu action for `ids` (the clicked track is
/// `ids[0]` for single selections).
pub fn run_track_menu(app: &mut App, host: &mut dyn Host, action: usize, ids: Vec<String>) {
    use crate::view::{Dialog, LibraryDetail, LibraryTab, Page};
    match action {
        0 => app.play_tracks(host, ids, 0),
        1 => {
            let n = ids.len();
            app.queue.play_next(ids);
            app.toast(format!("{n} {}", app.tr("will play next")), false);
            app.settings_changed();
        }
        2 => app.enqueue(host, ids),
        3 => app.view.dialog = Some(Dialog::AddToPlaylist { items: ids, kind: oc_net::api::PlaylistKind::Track }),
        4 => {
            if let Some(t) = ids.first().map(|i| app.track_for(i)) {
                app.view.page = Page::Library;
                app.view.library_tab = LibraryTab::Albums;
                app.view.library_detail = Some(LibraryDetail::Album(t.album_id.clone()));
            }
        }
        5 => {
            if let Some(t) = ids.first().map(|i| app.track_for(i)) {
                if let Some(a) = t.artists.first() {
                    app.view.page = Page::Library;
                    app.view.library_tab = LibraryTab::Artists;
                    app.view.library_detail = Some(LibraryDetail::Artist(a.clone()));
                }
            }
        }
        _ => {}
    }
}

/// Renders a virtualized list of tracks. Returns the clicked row index
/// (into `ids`) and the row index whose context menu was requested.
pub fn track_list(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, list_id: u64, ids: &[String], current: Option<usize>, numbered: bool) -> (Option<usize>, Option<usize>) {
    let t = ui.theme().clone();
    let row_h = if ui.input.touch { 56.0 } else { 48.0 };
    let wide = r.w > 760.0;
    let off = ui.begin_scroll(list_id, r, ids.len() as f32 * row_h + 8.0);
    let first = (off / row_h).floor().max(0.0) as usize;
    let last = (((off + r.h) / row_h).ceil() as usize + 1).min(ids.len());
    let mut clicked = None;
    let mut menu = None;
    for i in first..last {
        let rr = Rect::new(r.x, r.y + i as f32 * row_h - off, r.w - 10.0, row_h - 2.0);
        let resp = ui.interact(oc_ui::child(list_id, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, current == Some(i), i);
        let track = app.track_for(&ids[i]);
        let mut x = rr.x + 8.0;
        if numbered {
            let n = format!("{}", i + 1);
            ui.text(Rect::new(x, rr.y, 28.0, rr.h), &n, 12.0, Weight::Medium, t.text_faint, Align::Right);
            x += 38.0;
        }
        let cover = Rect::new(x, rr.y + (rr.h - 36.0) / 2.0, 36.0, 36.0);
        let key = if track.album_id.is_empty() { format!("track:{}", track.id) } else { format!("album:{}", track.album_id) };
        draw_cover(app, ui, host, cover, &key, &track.id, 5.0, 1.0);
        x = cover.right() + 12.0;
        let dur_w = 56.0;
        let text_w = rr.right() - x - dur_w - 8.0;
        let color = if current == Some(i) { t.primary_hover } else { t.text };
        if wide {
            let cols = Rect::new(x, rr.y, text_w, rr.h).columns(3, 12.0);
            ui.text(cols[0], &track.title, 14.0, Weight::Medium, color, Align::Left);
            ui.text(cols[1], &track.artist_line(), 13.0, Weight::Regular, t.text_dim, Align::Left);
            ui.text(cols[2], &track.album, 13.0, Weight::Regular, t.text_dim, Align::Left);
        } else {
            ui.text(Rect::new(x, rr.y + rr.h / 2.0 - 19.0, text_w, 20.0), &track.title, 14.0, Weight::Medium, color, Align::Left);
            let sub = if track.album.is_empty() { track.artist_line() } else { format!("{} · {}", track.artist_line(), track.album) };
            ui.text(Rect::new(x, rr.y + rr.h / 2.0 + 1.0, text_w, 18.0), &sub, 12.0, Weight::Regular, t.text_dim, Align::Left);
        }
        if track.duration_ms > 0 {
            ui.text(Rect::new(rr.right() - dur_w - 8.0, rr.y, dur_w, rr.h), &fmt_time(track.duration_ms as f64 / 1000.0), 12.0, Weight::Regular, t.text_faint, Align::Right);
        }
        if resp.clicked && !ui.is_drag_scrolling() {
            clicked = Some(i);
        }
        if secondary(ui, &resp, rr) {
            menu = Some(i);
        }
        if resp.hovered {
            ui.ctx.wants_pointer_cursor = true;
        }
    }
    ui.end_scroll(list_id, r);
    (clicked, menu)
}
