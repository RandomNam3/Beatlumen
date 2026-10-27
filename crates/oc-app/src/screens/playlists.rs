//! Playlists page (local and server playlists, tracks and backgrounds).

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::view::{ConfirmAction, Dialog, PromptAction};
use oc_net::api::{Playlist, PlaylistKind};
use oc_ui::{child, id, Align, Icon, MenuItem, Rect, RectExt, Ui, Weight};

fn all_playlists(app: &App) -> Vec<Playlist> {
    app.local_playlists.iter().chain(app.server_playlists.iter()).cloned().collect()
}

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    if let Some(open) = app.view.playlist_open.clone() {
        if let Some(pl) = all_playlists(app).into_iter().find(|p| p.id == open) {
            detail(app, ui, host, r, pl);
            return;
        }
        app.view.playlist_open = None;
    }
    let t = ui.theme().clone();
    let body = page_header(ui, r, app.tr("Playlists"), app.tr("Music and background playlists. Server playlists are shared with all your devices."));
    let (bar, rest) = body.split_top(46.0);
    let mut x = bar.x;
    if tool_button(ui, &mut x, bar.y, id("pl-new"), app.tr("New playlist"), Some(Icon::Plus), true) {
        app.view.dialog = Some(Dialog::Prompt { title: app.tr("New playlist").into(), value: String::new(), action: PromptAction::NewPlaylist { kind: PlaylistKind::Track, items: Vec::new() } });
    }
    if tool_button(ui, &mut x, bar.y, id("pl-new-bg"), app.tr("New background playlist"), Some(Icon::Image), false) {
        app.view.dialog = Some(Dialog::Prompt { title: app.tr("New background playlist").into(), value: String::new(), action: PromptAction::NewPlaylist { kind: PlaylistKind::Background, items: Vec::new() } });
    }
    let lists = all_playlists(app);
    if lists.is_empty() {
        empty_state(ui, rest, Icon::Playlist, app.tr("No playlists yet"), app.tr("Create a playlist or save the current queue as a playlist."));
        return;
    }
    let row_h = 56.0;
    let sid = id("pl-list");
    let off = ui.begin_scroll(sid, rest, lists.len() as f32 * row_h);
    let mut open = None;
    let mut menu = None;
    for (i, pl) in lists.iter().enumerate() {
        let rr = Rect::new(rest.x, rest.y + i as f32 * row_h - off, rest.w - 10.0, row_h - 2.0);
        if rr.bottom() < rest.y || rr.y > rest.bottom() {
            continue;
        }
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, false, i);
        let icon = if pl.kind == PlaylistKind::Background { Icon::Image } else { Icon::Playlist };
        ui.icon(Rect::new(rr.x + 10.0, rr.y, 30.0, rr.h), icon, 20.0, t.primary_hover);
        ui.text(Rect::new(rr.x + 52.0, rr.y + 8.0, rr.w - 200.0, 22.0), &pl.name, 15.0, Weight::SemiBold, t.text, Align::Left);
        let origin = if pl.id.starts_with("local-") { app.tr("on this device") } else { app.tr("on the server") };
        let sub = format!("{} {} · {origin}", pl.items.len(), if pl.kind == PlaylistKind::Background { app.tr("images") } else { app.tr("tracks") });
        ui.text(Rect::new(rr.x + 52.0, rr.y + 30.0, rr.w - 200.0, 18.0), &sub, 12.0, Weight::Regular, t.text_dim, Align::Left);
        if ui.icon_button(child(sid, 500_000 + i as u64), Rect::new(rr.right() - 46.0, rr.y + 10.0, 36.0, 36.0), Icon::Play, 18.0, false, app.tr("Play")).clicked {
            play_playlist(app, host, pl);
        } else if resp.clicked && !ui.is_drag_scrolling() {
            open = Some(pl.id.clone());
        }
        if secondary(ui, &resp, rr) {
            menu = Some(pl.id.clone());
        }
    }
    ui.end_scroll(sid, rest);
    if let Some(o) = open {
        app.view.playlist_open = Some(o);
    }
    if let Some(m) = menu {
        app.view.context_items = vec![m];
        let items = vec![
            MenuItem::new(app.tr("Play")).icon(Icon::Play),
            MenuItem::new(app.tr("Add to queue")).icon(Icon::Queue),
            MenuItem::new(app.tr("Rename…")).icon(Icon::Edit),
            MenuItem::new(app.tr("Delete")).icon(Icon::Trash).danger(),
        ];
        ui.open_context_menu(id("pl-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("pl-menu")) {
        let pid = app.view.context_items.first().cloned().unwrap_or_default();
        if let Some(pl) = all_playlists(app).into_iter().find(|p| p.id == pid) {
            playlist_action(app, host, &pl, a);
        }
    }
}

fn play_playlist(app: &mut App, host: &mut dyn Host, pl: &Playlist) {
    match pl.kind {
        PlaylistKind::Track => app.play_tracks(host, pl.items.clone(), 0),
        PlaylistKind::Background => {
            app.bg_queue.replace(pl.items.clone(), 0, &mut app.rng);
            app.show_queue_background(host);
            app.settings_changed();
        }
    }
}

fn playlist_action(app: &mut App, host: &mut dyn Host, pl: &Playlist, a: usize) {
    match a {
        0 => play_playlist(app, host, pl),
        1 => match pl.kind {
            PlaylistKind::Track => app.enqueue(host, pl.items.clone()),
            PlaylistKind::Background => app.add_backgrounds(host, pl.items.clone(), false),
        },
        2 => app.view.dialog = Some(Dialog::Prompt { title: app.tr("Rename playlist").into(), value: pl.name.clone(), action: PromptAction::RenamePlaylist(pl.id.clone()) }),
        3 => {
            app.view.dialog = Some(Dialog::Confirm {
                title: app.tr("Delete playlist").into(),
                message: format!("{} \"{}\"?", app.tr("Delete the playlist"), pl.name),
                action: ConfirmAction::DeletePlaylist(pl.id.clone()),
            })
        }
        _ => {}
    }
}

fn detail(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, pl: Playlist) {
    let t = ui.theme().clone();
    let (head, rest) = r.split_top(96.0);
    if ui.icon_button(id("pl-back"), Rect::new(head.x - 6.0, head.y, 36.0, 36.0), Icon::ChevronLeft, 22.0, false, app.tr("Back")).clicked {
        app.view.playlist_open = None;
    }
    ui.text(Rect::new(head.x + 40.0, head.y, head.w - 40.0, 36.0), &pl.name, 24.0, Weight::Bold, t.text, Align::Left);
    let mut x = head.x + 40.0;
    let by = head.y + 48.0;
    if tool_button(ui, &mut x, by, id("pld-play"), app.tr("Play"), Some(Icon::Play), true) {
        play_playlist(app, host, &pl);
    }
    if tool_button(ui, &mut x, by, id("pld-enqueue"), app.tr("Add to queue"), Some(Icon::Queue), false) {
        playlist_action(app, host, &pl, 1);
    }
    if tool_button(ui, &mut x, by, id("pld-rename"), app.tr("Rename"), Some(Icon::Edit), false) {
        playlist_action(app, host, &pl, 2);
    }
    if pl.items.is_empty() {
        let msg = if pl.kind == PlaylistKind::Track { app.tr("Add tracks from the library with the context menu (right click or long press).") } else { app.tr("Add images on the Backgrounds page.") };
        empty_state(ui, rest, Icon::Playlist, app.tr("This playlist is empty"), msg);
        return;
    }
    match pl.kind {
        PlaylistKind::Track => {
            let cur = app.current.as_ref().and_then(|c| pl.items.iter().position(|i| *i == c.id));
            let (clicked, menu) = track_list(app, ui, host, rest, child(id("pl-tracks"), oc_ui::id(&pl.id)), &pl.items, cur, true);
            if let Some(i) = clicked {
                app.play_tracks(host, pl.items.clone(), i);
            }
            if let Some(i) = menu {
                app.view.context_items = vec![i.to_string()];
                let items = vec![
                    MenuItem::new(app.tr("Play")).icon(Icon::Play),
                    MenuItem::new(app.tr("Move up")).icon(Icon::ChevronUp).enabled(i > 0),
                    MenuItem::new(app.tr("Move down")).icon(Icon::ChevronDown).enabled(i + 1 < pl.items.len()),
                    MenuItem::new(app.tr("Remove from playlist")).icon(Icon::Trash).danger(),
                ];
                ui.open_context_menu(id("pld-menu"), items);
            }
        }
        PlaylistKind::Background => {
            let row_h = 48.0;
            let sid = child(id("pl-images"), oc_ui::id(&pl.id));
            let off = ui.begin_scroll(sid, rest, pl.items.len() as f32 * row_h);
            let mut menu = None;
            for (i, item) in pl.items.iter().enumerate() {
                let rr = Rect::new(rest.x, rest.y + i as f32 * row_h - off, rest.w - 10.0, row_h - 2.0);
                if rr.bottom() < rest.y || rr.y > rest.bottom() {
                    continue;
                }
                let resp = ui.interact(child(sid, i as u64 + 1), rr);
                row_background(ui, rr, resp.hovered, false, i);
                ui.icon(Rect::new(rr.x + 8.0, rr.y, 28.0, rr.h), Icon::Image, 18.0, t.text_dim);
                let label = super::queue::background_label(app, item);
                ui.text(Rect::new(rr.x + 46.0, rr.y, rr.w - 56.0, rr.h), &label, 14.0, Weight::Regular, t.text, Align::Left);
                if secondary(ui, &resp, rr) || resp.clicked {
                    menu = Some(i);
                }
            }
            ui.end_scroll(sid, rest);
            if let Some(i) = menu {
                app.view.context_items = vec![i.to_string()];
                let items = vec![
                    MenuItem::new(app.tr("Show")).icon(Icon::Eye),
                    MenuItem::new(app.tr("Move up")).icon(Icon::ChevronUp).enabled(i > 0),
                    MenuItem::new(app.tr("Move down")).icon(Icon::ChevronDown).enabled(i + 1 < pl.items.len()),
                    MenuItem::new(app.tr("Remove from playlist")).icon(Icon::Trash).danger(),
                ];
                ui.open_context_menu(id("pld-menu"), items);
            }
        }
    }
    if let Some(a) = ui.menu_result(id("pld-menu")) {
        let i: usize = app.view.context_items.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let mut p = pl.clone();
        match a {
            0 => match pl.kind {
                PlaylistKind::Track => app.play_tracks(host, pl.items.clone(), i),
                PlaylistKind::Background => {
                    if let Some(s) = pl.items.get(i) {
                        app.add_backgrounds(host, vec![s.clone()], true);
                    }
                }
            },
            1 if i > 0 => {
                p.items.swap(i, i - 1);
                app.update_playlist(host, &p);
            }
            2 if i + 1 < p.items.len() => {
                p.items.swap(i, i + 1);
                app.update_playlist(host, &p);
            }
            3 => {
                if i < p.items.len() {
                    p.items.remove(i);
                    app.update_playlist(host, &p);
                }
            }
            _ => {}
        }
    }
}
