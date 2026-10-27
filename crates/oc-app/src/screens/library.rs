//! Library page: tracks, albums, artists and folders.

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::view::{BrowseFor, Dialog, FileBrowser, LibraryDetail, LibraryTab};
use oc_ui::{child, id, Align, Color, Icon, MenuItem, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let stats = format!("{} {} · {} {} · {} {}", app.library.tracks.len(), app.tr("tracks"), app.library.albums.len(), app.tr("albums"), app.library.artists.len(), app.tr("artists"));
    let body = page_header(ui, r, app.tr("Library"), &stats);
    // Toolbar: tabs + search + actions.
    let compact = body.w < 640.0;
    let (bar, rest) = body.split_top(if compact { 84.0 } else { 42.0 });
    let tabs_w = if compact { bar.w } else { (bar.w * 0.45).min(420.0) };
    let labels = [app.tr("Tracks"), app.tr("Albums"), app.tr("Artists"), app.tr("Folders")];
    let mut tab = match app.view.library_tab {
        LibraryTab::Tracks => 0,
        LibraryTab::Albums => 1,
        LibraryTab::Artists => 2,
        LibraryTab::Folders => 3,
    };
    if ui.tabs(id("lib-tabs"), Rect::new(bar.x, bar.y, tabs_w, 36.0), &mut tab, &labels) {
        app.view.library_tab = [LibraryTab::Tracks, LibraryTab::Albums, LibraryTab::Artists, LibraryTab::Folders][tab];
        app.view.library_detail = None;
        app.view.folder_path.clear();
    }
    let search_r = if compact { Rect::new(bar.x, bar.y + 44.0, bar.w - 50.0, 36.0) } else { Rect::new(bar.x + tabs_w + 12.0, bar.y, bar.w - tabs_w - 64.0, 36.0) };
    let sid = id("lib-search");
    if app.view.focus_search {
        ui.ctx.set_focus(sid);
        app.view.focus_search = false;
    }
    let ph = app.tr("Search title, artist, album…").to_string();
    let (changed, _) = ui.text_input(sid, search_r, &mut app.view.search, &ph, false);
    if changed && !app.view.search.is_empty() && app.view.library_tab != LibraryTab::Tracks {
        app.view.library_tab = LibraryTab::Tracks;
        app.view.library_detail = None;
    }
    // Actions menu.
    let more_r = Rect::new(search_r.right() + 8.0, search_r.y, 40.0, 36.0);
    if ui.icon_button(id("lib-more"), more_r, Icon::More, 20.0, false, app.tr("More")).clicked {
        let mut items = Vec::new();
        items.push(MenuItem::new(app.tr("Add music folder…")).icon(Icon::Folder).enabled(host.has_filesystem()));
        items.push(MenuItem::new(app.tr("Open files…")).icon(Icon::File));
        items.push(MenuItem::new(app.tr("Rescan local folders")).icon(Icon::Refresh).enabled(!app.settings.library_folders.is_empty()));
        items.push(MenuItem::new(app.tr("Rescan server library")).icon(Icon::Server).enabled(app.server.is_connected()));
        items.push(MenuItem::new(app.tr("Play all")).icon(Icon::Play));
        items.push(MenuItem::new(app.tr("Shuffle all")).icon(Icon::Shuffle));
        ui.open_menu(id("lib-more"), more_r, items, 240.0);
    }
    if let Some(i) = ui.menu_result(id("lib-more")) {
        match i {
            0 => app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::LibraryFolder, path: start_dir(host), selected: Vec::new() })),
            1 => {
                if host.has_filesystem() {
                    app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::AudioFiles, path: start_dir(host), selected: Vec::new() }));
                } else {
                    host.pick_files(".mp3,.flac,.ogg,.oga,.opus,.m4a,.aac,.wav,.aif,.aiff,.mka,.webm,audio/*", true);
                }
            }
            2 => app.rescan_local(),
            3 => app.server_rescan(host),
            4 | 5 => {
                let ids: Vec<String> = app.library.tracks.iter().map(|t| t.id.clone()).collect();
                if i == 5 && !app.queue.shuffle {
                    app.queue.set_shuffle(true, &mut app.rng);
                }
                app.play_tracks(host, ids, 0);
            }
            _ => {}
        }
    }
    let content = rest.pad(0.0, 4.0);
    // Scan progress.
    let scan = app.library.scan.lock().map(|s| s.clone()).unwrap_or_default();
    let content = if scan.running {
        let (top, rest) = content.split_top(30.0);
        let msg = format!("{} {}/{}", app.tr("Scanning library…"), scan.processed, scan.found);
        ui.text(top, &msg, 12.0, Weight::Medium, t.text_dim, Align::Left);
        let f = if scan.found > 0 { scan.processed as f32 / scan.found as f32 } else { 0.0 };
        ui.rect(Rect::new(top.x, top.bottom() - 4.0, top.w, 2.0), t.surface, 1.0);
        ui.rect(Rect::new(top.x, top.bottom() - 4.0, top.w * f, 2.0), t.primary, 1.0);
        rest
    } else {
        content
    };
    if app.library.tracks.is_empty() {
        let msg = if host.has_filesystem() {
            app.tr("Add a music folder with the ⋯ menu, drop audio files onto the window, or connect to an OpenChroma server in the settings.")
        } else {
            app.tr("Connect to an OpenChroma server in the settings or open audio files with the ⋯ menu.")
        };
        empty_state(ui, content, Icon::Music, app.tr("Your library is empty"), msg);
        return;
    }
    match app.view.library_detail.clone() {
        Some(LibraryDetail::Album(a)) => album_detail(app, ui, host, content, &a),
        Some(LibraryDetail::Artist(a)) => artist_detail(app, ui, host, content, &a),
        None => match app.view.library_tab {
            LibraryTab::Tracks => tracks(app, ui, host, content),
            LibraryTab::Albums => albums(app, ui, host, content),
            LibraryTab::Artists => artists(app, ui, content),
            LibraryTab::Folders => folders(app, ui, host, content),
        },
    }
}

fn start_dir(host: &dyn Host) -> String {
    host.fs_roots().into_iter().find(|(l, _)| l == "Music").or_else(|| host.fs_roots().into_iter().next()).map(|(_, p)| p).unwrap_or_else(|| "/".into())
}

fn filtered(app: &mut App) -> Vec<usize> {
    let rev = app.library.revision;
    let q = app.view.search.clone();
    if let Some((r, cq, v)) = &app.view.filter_cache {
        if *r == rev && *cq == q {
            return v.clone();
        }
    }
    let v = app.library.search(&q);
    app.view.filter_cache = Some((rev, q, v.clone()));
    v
}

fn tracks(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let idx = filtered(app);
    let ids: Vec<String> = idx.iter().map(|&i| app.library.tracks[i].id.clone()).collect();
    if ids.is_empty() {
        empty_state(ui, r, Icon::Search, app.tr("No results"), app.tr("Try a different search."));
        return;
    }
    let cur = app.current.as_ref().and_then(|c| ids.iter().position(|i| *i == c.id));
    let (clicked, menu) = track_list(app, ui, host, r, id("lib-tracks"), &ids, cur, false);
    if let Some(i) = clicked {
        app.play_tracks(host, ids.clone(), i);
    }
    if let Some(i) = menu {
        app.view.context_items = vec![ids[i].clone()];
        let items = track_menu_items(app);
        ui.open_context_menu(id("lib-track-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("lib-track-menu")) {
        let items = std::mem::take(&mut app.view.context_items);
        run_track_menu(app, host, a, items);
    }
}

fn albums(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let cell = if r.w < 500.0 { (r.w - 20.0) / 2.0 } else { 168.0 };
    let cols = (((r.w - 10.0) / (cell + 16.0)).floor() as usize).max(1);
    let cell_w = (r.w - 10.0 - (cols - 1) as f32 * 16.0) / cols as f32;
    let cell_h = cell_w + 52.0;
    let rows = app.library.albums.len().div_ceil(cols);
    let sid = id("lib-albums");
    let off = ui.begin_scroll(sid, r, rows as f32 * (cell_h + 12.0));
    let first_row = (off / (cell_h + 12.0)).floor().max(0.0) as usize;
    let last_row = (((off + r.h) / (cell_h + 12.0)).ceil() as usize + 1).min(rows);
    let mut open = None;
    let mut menu = None;
    for row in first_row..last_row {
        for c in 0..cols {
            let i = row * cols + c;
            let Some(album) = app.library.albums.get(i).cloned() else { break };
            let cr = Rect::new(r.x + c as f32 * (cell_w + 16.0), r.y + row as f32 * (cell_h + 12.0) - off, cell_w, cell_h);
            let resp = ui.interact(child(sid, i as u64 + 1), cr);
            let h = ui.anim(child(sid, 900_000 + i as u64), if resp.hovered { 1.0 } else { 0.0 }, 14.0);
            let cover = Rect::new(cr.x, cr.y, cell_w, cell_w);
            if h > 0.0 {
                ui.shadow(cover, Color(0.0, 0.0, 0.0, 0.4 * h), 14.0);
            }
            let key = format!("album:{}", album.id);
            let track_id = album.cover_track.clone().unwrap_or_default();
            draw_cover(app, ui, host, cover, &key, &track_id, 8.0, 1.0);
            ui.text(Rect::new(cr.x, cover.bottom() + 6.0, cr.w, 20.0), &album.name, 14.0, Weight::SemiBold, t.text, Align::Left);
            let sub = match album.year {
                Some(y) => format!("{} · {y}", album.artist),
                None => album.artist.clone(),
            };
            ui.text(Rect::new(cr.x, cover.bottom() + 26.0, cr.w, 18.0), &sub, 12.0, Weight::Regular, t.text_dim, Align::Left);
            if resp.clicked && !ui.is_drag_scrolling() {
                open = Some(album.id.clone());
            }
            if secondary(ui, &resp, cr) {
                menu = Some(album.id.clone());
            }
            if resp.hovered {
                ui.ctx.wants_pointer_cursor = true;
            }
        }
    }
    ui.end_scroll(sid, r);
    if let Some(a) = open {
        app.view.library_detail = Some(LibraryDetail::Album(a));
    }
    if let Some(a) = menu {
        app.view.context_items = app.library.album_tracks(&a).iter().map(|t| t.id.clone()).collect();
        let items = track_menu_items(app)[..4].to_vec();
        ui.open_context_menu(id("lib-album-menu"), items);
    }
    if let Some(action) = ui.menu_result(id("lib-album-menu")) {
        let items = std::mem::take(&mut app.view.context_items);
        run_track_menu(app, host, action, items);
    }
}

fn detail_header(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, title: &str, subtitle: &str, cover: Option<(String, String)>, ids: &[String]) -> Rect {
    let t = ui.theme().clone();
    let compact = r.w < 560.0;
    let head_h = if compact { 120.0 } else { 168.0 };
    let (head, rest) = r.split_top(head_h + 16.0);
    if ui.icon_button(id("detail-back"), Rect::new(head.x - 6.0, head.y, 36.0, 36.0), Icon::ChevronLeft, 22.0, false, app.tr("Back")).clicked {
        app.view.library_detail = None;
    }
    let mut x = head.x + 40.0;
    if let Some((key, track)) = cover {
        let cr = Rect::new(x, head.y, head_h, head_h);
        ui.shadow(cr, Color(0.0, 0.0, 0.0, 0.35), 16.0);
        draw_cover(app, ui, host, cr, &key, &track, 10.0, 1.0);
        x = cr.right() + 20.0;
    }
    let w = head.right() - x;
    ui.text(Rect::new(x, head.y + 8.0, w, 32.0), title, if compact { 20.0 } else { 26.0 }, Weight::Bold, t.text, Align::Left);
    ui.text(Rect::new(x, head.y + 42.0, w, 20.0), subtitle, 13.0, Weight::Regular, t.text_dim, Align::Left);
    let mut bx = x;
    let by = head.y + head_h - 36.0;
    if tool_button(ui, &mut bx, by, id("detail-play"), app.tr("Play"), Some(Icon::Play), true) {
        app.play_tracks(host, ids.to_vec(), 0);
    }
    if tool_button(ui, &mut bx, by, id("detail-shuffle"), app.tr("Shuffle"), Some(Icon::Shuffle), false) {
        if !app.queue.shuffle {
            app.queue.set_shuffle(true, &mut app.rng);
        }
        app.play_tracks(host, ids.to_vec(), 0);
    }
    if !compact && tool_button(ui, &mut bx, by, id("detail-enqueue"), app.tr("Add to queue"), Some(Icon::Queue), false) {
        app.enqueue(host, ids.to_vec());
    }
    rest
}

fn album_detail(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, album_id: &str) {
    let Some(album) = app.library.album(album_id).cloned() else {
        app.view.library_detail = None;
        return;
    };
    let ids: Vec<String> = app.library.album_tracks(album_id).iter().map(|t| t.id.clone()).collect();
    let mut sub = album.artist.clone();
    if let Some(y) = album.year {
        sub.push_str(&format!(" · {y}"));
    }
    sub.push_str(&format!(" · {} {} · {}", ids.len(), app.tr("tracks"), fmt_time(album.duration_ms as f64 / 1000.0)));
    let cover = Some((format!("album:{}", album.id), album.cover_track.clone().unwrap_or_default()));
    let list_r = detail_header(app, ui, host, r, &album.name, &sub, cover, &ids);
    let cur = app.current.as_ref().and_then(|c| ids.iter().position(|i| *i == c.id));
    let (clicked, menu) = track_list(app, ui, host, list_r, id("album-tracks"), &ids, cur, true);
    if let Some(i) = clicked {
        app.play_tracks(host, ids.clone(), i);
    }
    if let Some(i) = menu {
        app.view.context_items = vec![ids[i].clone()];
        let items = track_menu_items(app);
        ui.open_context_menu(id("album-track-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("album-track-menu")) {
        let items = std::mem::take(&mut app.view.context_items);
        run_track_menu(app, host, a, items);
    }
}

fn artists(app: &mut App, ui: &mut Ui, r: Rect) {
    let t = ui.theme().clone();
    let row_h = 52.0;
    let sid = id("lib-artists");
    let n = app.library.artists.len();
    let off = ui.begin_scroll(sid, r, n as f32 * row_h);
    let first = (off / row_h) as usize;
    let last = (((off + r.h) / row_h).ceil() as usize + 1).min(n);
    let mut open = None;
    for i in first..last {
        let a = app.library.artists[i].clone();
        let rr = Rect::new(r.x, r.y + i as f32 * row_h - off, r.w - 10.0, row_h - 2.0);
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, false, i);
        let av = Rect::new(rr.x + 8.0, rr.y + 6.0, 38.0, 38.0);
        ui.rect(av, t.surface_active, 19.0);
        let initial: String = a.name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        ui.text(av, &initial, 16.0, Weight::Bold, t.text_dim, Align::Center);
        ui.text(Rect::new(av.right() + 14.0, rr.y, rr.w - 260.0, rr.h), &a.name, 14.0, Weight::Medium, t.text, Align::Left);
        let stats = format!("{} {} · {} {}", a.albums, app.tr("albums"), a.tracks, app.tr("tracks"));
        ui.text(Rect::new(rr.right() - 200.0, rr.y, 190.0, rr.h), &stats, 12.0, Weight::Regular, t.text_faint, Align::Right);
        if resp.clicked && !ui.is_drag_scrolling() {
            open = Some(a.name.clone());
        }
        if resp.hovered {
            ui.ctx.wants_pointer_cursor = true;
        }
    }
    ui.end_scroll(sid, r);
    if let Some(a) = open {
        app.view.library_detail = Some(LibraryDetail::Artist(a));
    }
}

fn artist_detail(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, name: &str) {
    let ids: Vec<String> = app.library.artist_tracks(name).iter().map(|t| t.id.clone()).collect();
    let sub = format!("{} {}", ids.len(), app.tr("tracks"));
    let cover = ids.first().map(|i| {
        let t = app.track_for(i);
        (format!("album:{}", t.album_id), t.id.clone())
    });
    let list_r = detail_header(app, ui, host, r, name, &sub, cover, &ids);
    let cur = app.current.as_ref().and_then(|c| ids.iter().position(|i| *i == c.id));
    let (clicked, menu) = track_list(app, ui, host, list_r, id("artist-tracks"), &ids, cur, false);
    if let Some(i) = clicked {
        app.play_tracks(host, ids.clone(), i);
    }
    if let Some(i) = menu {
        app.view.context_items = vec![ids[i].clone()];
        let items = track_menu_items(app);
        ui.open_context_menu(id("artist-track-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("artist-track-menu")) {
        let items = std::mem::take(&mut app.view.context_items);
        run_track_menu(app, host, a, items);
    }
}

/// Folder view built from track paths.
fn folders(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let prefix: Vec<String> = app.view.folder_path.clone();
    // Split each track path into components (local absolute paths are
    // grouped under their library folder).
    let mut subdirs: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for tr in &app.library.tracks {
        let comps = folder_components(app, tr);
        if comps.len() <= prefix.len() || comps[..prefix.len()] != prefix[..] {
            continue;
        }
        if comps.len() == prefix.len() + 1 {
            files.push(tr.id.clone());
        } else {
            let d = comps[prefix.len()].clone();
            if !subdirs.contains(&d) {
                subdirs.push(d);
            }
        }
    }
    subdirs.sort_by_key(|a| a.to_lowercase());
    let (head, rest) = r.split_top(40.0);
    let mut x = head.x;
    if !prefix.is_empty() && ui.icon_button(id("folder-up"), Rect::new(x - 6.0, head.y, 36.0, 36.0), Icon::ChevronLeft, 20.0, false, app.tr("Up")).clicked {
        app.view.folder_path.pop();
    }
    x += 36.0;
    let crumb = if prefix.is_empty() { app.tr("All folders").to_string() } else { prefix.join(" / ") };
    ui.text(Rect::new(x, head.y, head.w - 200.0, 36.0), &crumb, 14.0, Weight::SemiBold, t.text, Align::Left);
    if !files.is_empty() {
        let mut right = head.right();
        if tool_button_right(ui, &mut right, head.y, id("folder-play"), app.tr("Play folder"), Some(Icon::Play), false) {
            app.play_tracks(host, files.clone(), 0);
        }
    }
    let row_h = 48.0;
    let n = subdirs.len() + files.len();
    let sid = id("folders");
    let off = ui.begin_scroll(sid, rest, n as f32 * row_h);
    let mut enter = None;
    let mut play = None;
    for i in (off / row_h) as usize..((((off + rest.h) / row_h).ceil() as usize) + 1).min(n) {
        let rr = Rect::new(rest.x, rest.y + i as f32 * row_h - off, rest.w - 10.0, row_h - 2.0);
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, false, i);
        if i < subdirs.len() {
            ui.icon(Rect::new(rr.x + 8.0, rr.y, 28.0, rr.h), Icon::Folder, 20.0, t.primary_hover);
            ui.text(Rect::new(rr.x + 46.0, rr.y, rr.w - 56.0, rr.h), &subdirs[i], 14.0, Weight::Medium, t.text, Align::Left);
            if resp.clicked && !ui.is_drag_scrolling() {
                enter = Some(subdirs[i].clone());
            }
        } else {
            let tr = app.track_for(&files[i - subdirs.len()]);
            ui.icon(Rect::new(rr.x + 8.0, rr.y, 28.0, rr.h), Icon::Music, 18.0, t.text_dim);
            ui.text(Rect::new(rr.x + 46.0, rr.y, rr.w - 120.0, rr.h), &tr.title, 14.0, Weight::Regular, t.text, Align::Left);
            ui.text(Rect::new(rr.right() - 70.0, rr.y, 60.0, rr.h), &fmt_time(tr.duration_ms as f64 / 1000.0), 12.0, Weight::Regular, t.text_faint, Align::Right);
            if resp.clicked && !ui.is_drag_scrolling() {
                play = Some(i - subdirs.len());
            }
        }
        if resp.hovered {
            ui.ctx.wants_pointer_cursor = true;
        }
    }
    ui.end_scroll(sid, rest);
    if let Some(d) = enter {
        app.view.folder_path.push(d);
    }
    if let Some(i) = play {
        app.play_tracks(host, files, i);
    }
}

fn folder_components(app: &App, tr: &oc_net::api::Track) -> Vec<String> {
    if let Some(path) = crate::library::local_path(&tr.id) {
        let path = path.replace('\\', "/");
        for f in &app.settings.library_folders {
            let f = f.replace('\\', "/");
            let f = f.trim_end_matches('/');
            if let Some(rel) = path.strip_prefix(f) {
                let root = f.rsplit('/').next().unwrap_or(f).to_string();
                let mut v = vec![root];
                v.extend(rel.split('/').filter(|s| !s.is_empty()).map(str::to_string));
                return v;
            }
        }
        path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect()
    } else {
        let mut v = vec!["Server".to_string()];
        v.extend(tr.path.split('/').filter(|s| !s.is_empty()).map(str::to_string));
        v
    }
}
