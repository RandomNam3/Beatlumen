//! Queue page: audio queue and background queue.

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::settings::{BackgroundMode, Repeat};
use crate::view::{ConfirmAction, Dialog};
use oc_ui::{child, id, Align, Icon, MenuItem, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let body = page_header(ui, r, app.tr("Queue"), "");
    let (bar, rest) = body.split_top(46.0);
    let labels = [app.tr("Music"), app.tr("Backgrounds")];
    let w = (bar.w * 0.5).min(320.0);
    ui.tabs(id("queue-tabs"), Rect::new(bar.x, bar.y, w, 36.0), &mut app.view.queue_tab, &labels);
    if app.view.queue_tab == 0 {
        audio(app, ui, host, bar, rest);
    } else {
        backgrounds(app, ui, host, bar, rest);
    }
}

fn audio(app: &mut App, ui: &mut Ui, host: &mut dyn Host, bar: Rect, rest: Rect) {
    let mut right = bar.right();
    if tool_button_right(ui, &mut right, bar.y, id("q-clear"), app.tr("Clear"), Some(Icon::Trash), false) {
        app.view.dialog = Some(Dialog::Confirm { title: app.tr("Clear queue").into(), message: app.tr("Remove all tracks from the queue?").into(), action: ConfirmAction::ClearQueue });
    }
    if tool_button_right(ui, &mut right, bar.y, id("q-save"), app.tr("Save as playlist"), Some(Icon::Save), false) && !app.queue.is_empty() {
        let items: Vec<String> = app.queue.ordered().map(|(_, s)| s.clone()).collect();
        app.view.dialog = Some(Dialog::Prompt {
            title: app.tr("New playlist").into(),
            value: String::new(),
            action: crate::view::PromptAction::NewPlaylist { kind: oc_net::api::PlaylistKind::Track, items },
        });
    }
    if app.queue.is_empty() {
        empty_state(ui, rest, Icon::Queue, app.tr("The queue is empty"), app.tr("Play something from the library or drop audio files onto the window."));
        return;
    }
    let ids: Vec<String> = app.queue.ordered().map(|(_, s)| s.clone()).collect();
    let cur = app.queue.position();
    let (clicked, menu) = track_list(app, ui, host, rest, id("queue-list"), &ids, cur, true);
    if let Some(i) = clicked {
        app.queue.set_position(i);
        app.play_current(host, true);
    }
    if let Some(i) = menu {
        app.view.context_items = vec![i.to_string()];
        let items = vec![
            MenuItem::new(app.tr("Play")).icon(Icon::Play),
            MenuItem::new(app.tr("Move up")).icon(Icon::ChevronUp).enabled(i > 0),
            MenuItem::new(app.tr("Move down")).icon(Icon::ChevronDown).enabled(i + 1 < ids.len()),
            MenuItem::new(app.tr("Add to playlist…")).icon(Icon::Playlist),
            MenuItem::new(app.tr("Remove from queue")).icon(Icon::Trash).danger(),
        ];
        ui.open_context_menu(id("queue-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("queue-menu")) {
        let i: usize = app.view.context_items.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        match a {
            0 => {
                app.queue.set_position(i);
                app.play_current(host, true);
            }
            1 => app.queue.move_entry(i, i.saturating_sub(1)),
            2 => app.queue.move_entry(i, i + 1),
            3 => {
                if let Some(id) = ids.get(i) {
                    app.view.dialog = Some(Dialog::AddToPlaylist { items: vec![id.clone()], kind: oc_net::api::PlaylistKind::Track });
                }
            }
            4 => {
                let was_current = app.queue.position() == Some(i);
                app.queue.remove_at(i);
                if was_current {
                    app.play_current(host, app.is_playing());
                }
            }
            _ => {}
        }
        app.settings_changed();
    }
}

fn backgrounds(app: &mut App, ui: &mut Ui, host: &mut dyn Host, bar: Rect, rest: Rect) {
    let t = ui.theme().clone();
    let mut right = bar.right();
    if tool_button_right(ui, &mut right, bar.y, id("bq-clear"), app.tr("Clear"), Some(Icon::Trash), false) {
        app.view.dialog = Some(Dialog::Confirm { title: app.tr("Clear backgrounds").into(), message: app.tr("Remove all images from the background queue?").into(), action: ConfirmAction::ClearBackgrounds });
    }
    // Background options.
    let (opts, list_r) = rest.split_top(if rest.w < 560.0 { 126.0 } else { 48.0 });
    let modes: Vec<String> = BackgroundMode::ALL.iter().map(|m| app.tr(mode_label(*m)).to_string()).collect();
    let mut mi = BackgroundMode::ALL.iter().position(|m| *m == app.settings.background_mode).unwrap_or(2);
    let cols = if rest.w < 560.0 { vec![opts.row(0, 40.0), opts.row(1, 40.0), opts.row(2, 40.0)] } else { opts.split_top(40.0).0.columns(3, 12.0) };
    ui.text(Rect::new(cols[0].x, cols[0].y, 90.0, 34.0), app.tr("Advance"), 13.0, Weight::Regular, t.text_dim, Align::Left);
    if ui.select(id("bq-mode"), Rect::new(cols[0].x + 90.0, cols[0].y, cols[0].w - 90.0, 34.0), &mut mi, &modes) {
        app.settings.background_mode = BackgroundMode::ALL[mi];
        app.settings_changed();
    }
    let mut shuffle = app.bg_queue.shuffle;
    if ui.checkbox(id("bq-shuffle"), cols[1].split_top(34.0).0, &mut shuffle, app.tr("Shuffle")) {
        app.bg_queue.set_shuffle(shuffle, &mut app.rng);
        app.settings_changed();
    }
    let reps = vec![app.tr("No repeat").to_string(), app.tr("Repeat queue").to_string(), app.tr("Repeat image").to_string()];
    let mut ri = match app.settings.background_repeat {
        Repeat::None => 0,
        Repeat::Queue => 1,
        Repeat::Item => 2,
    };
    if ui.select(id("bq-repeat"), cols[2].split_top(34.0).0, &mut ri, &reps) {
        app.settings.background_repeat = [Repeat::None, Repeat::Queue, Repeat::Item][ri];
        app.settings_changed();
    }
    if app.bg_queue.is_empty() {
        let msg = if app.settings.cover_fallback { app.tr("Add images on the Backgrounds page. Until then the album cover of the current track is used.") } else { app.tr("Add images on the Backgrounds page.") };
        empty_state(ui, list_r, Icon::Image, app.tr("No background images"), msg);
        return;
    }
    let entries: Vec<String> = app.bg_queue.ordered().map(|(_, s)| s.clone()).collect();
    let cur = app.bg_queue.position();
    let row_h = 52.0;
    let sid = id("bq-list");
    let off = ui.begin_scroll(sid, list_r, entries.len() as f32 * row_h);
    let mut play = None;
    let mut remove = None;
    for i in (off / row_h) as usize..((((off + list_r.h) / row_h).ceil() as usize) + 1).min(entries.len()) {
        let rr = Rect::new(list_r.x, list_r.y + i as f32 * row_h - off, list_r.w - 10.0, row_h - 2.0);
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, cur == Some(i), i);
        ui.icon(Rect::new(rr.x + 8.0, rr.y, 28.0, rr.h), Icon::Image, 18.0, t.text_dim);
        let label = background_label(app, &entries[i]);
        ui.text(Rect::new(rr.x + 46.0, rr.y, rr.w - 100.0, rr.h), &label, 14.0, Weight::Regular, if cur == Some(i) { t.primary_hover } else { t.text }, Align::Left);
        if ui.icon_button(child(sid, 1_000_000 + i as u64), Rect::new(rr.right() - 44.0, rr.y + (rr.h - 36.0) / 2.0, 36.0, 36.0), Icon::Close, 16.0, false, app.tr("Remove")).clicked {
            remove = Some(i);
        } else if resp.clicked && !ui.is_drag_scrolling() {
            play = Some(i);
        }
    }
    ui.end_scroll(sid, list_r);
    if let Some(i) = play {
        app.bg_queue.set_position(i);
        app.show_queue_background(host);
        app.settings_changed();
    }
    if let Some(i) = remove {
        let was_current = app.bg_queue.position() == Some(i);
        app.bg_queue.remove_at(i);
        if was_current {
            app.show_queue_background(host);
        }
        app.settings_changed();
    }
}

pub fn mode_label(m: BackgroundMode) -> &'static str {
    match m {
        BackgroundMode::Manual => "Manually",
        BackgroundMode::OnTrackFinish => "When a track finishes",
        BackgroundMode::OnTrackChange => "On every track change",
    }
}

/// Human readable name of a background source.
pub fn background_label(app: &App, source: &str) -> String {
    if let Some(p) = crate::library::local_path(source) {
        return p.rsplit(['/', '\\']).next().unwrap_or(p).to_string();
    }
    if let Some(id) = source.strip_prefix("srv:") {
        if let Some(e) = app.server_images.iter().find(|e| e.id == id) {
            return e.name.clone();
        }
        return format!("{} {id}", app.tr("Server image"));
    }
    if let Some(n) = source.strip_prefix("mem:") {
        return n.to_string();
    }
    source.to_string()
}
