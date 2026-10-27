//! Modal dialogs: prompts, confirmations, file browser, playlist picker.

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::library::{is_image_extension, LOCAL_PREFIX};
use crate::view::{BrowseFor, ConfirmAction, Dialog, PromptAction};
use bl_net::api::PlaylistKind;
use bl_ui::{child, id, Align, Color, Icon, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host) {
    let Some(dialog) = app.view.dialog.clone() else { return };
    let screen = ui.screen();
    ui.set_layer(5);
    ui.rect(screen, ui.theme().backdrop, 0.0);
    ui.block(screen);
    let t = ui.theme().clone();
    let (w, h) = match &dialog {
        Dialog::FileBrowser(_) => (screen.w.min(760.0) - 24.0, screen.h.min(640.0) - 24.0),
        Dialog::AddToPlaylist { .. } => (screen.w.min(440.0) - 24.0, screen.h.min(520.0) - 24.0),
        _ => (screen.w.min(460.0) - 24.0, 210.0),
    };
    let r = screen.centered(w, h);
    ui.shadow(r, t.shadow, 30.0);
    ui.rect(r, t.panel_strong, t.radius + 2.0);
    ui.outline(r, t.border, t.radius + 2.0, 1.0);
    ui.block(r);
    let inner = r.pad(20.0, 18.0);
    let close = match dialog {
        Dialog::Prompt { title, mut value, action } => {
            ui.text(Rect::new(inner.x, inner.y, inner.w, 28.0), &title, 18.0, Weight::Bold, t.text, Align::Left);
            let fid = id("dialog-prompt");
            if ui.ctx.focused() != fid && !ui.input.pressed[0] {
                ui.ctx.set_focus(fid);
            }
            let (_, submit) = ui.text_input(fid, Rect::new(inner.x, inner.y + 48.0, inner.w, 38.0), &mut value, "", false);
            let (ok, cancel) = buttons(app, ui, inner);
            if (ok || submit) && !value.trim().is_empty() {
                run_prompt(app, host, action, value.trim().to_string());
                true
            } else {
                if let Some(Dialog::Prompt { value: v, .. }) = &mut app.view.dialog {
                    *v = value;
                }
                cancel
            }
        }
        Dialog::SavePresetAs(mut name) => {
            ui.text(Rect::new(inner.x, inner.y, inner.w, 28.0), app.tr("Save preset as"), 18.0, Weight::Bold, t.text, Align::Left);
            let fid = id("dialog-preset-name");
            if ui.ctx.focused() != fid && !ui.input.pressed[0] {
                ui.ctx.set_focus(fid);
            }
            let (_, submit) = ui.text_input(fid, Rect::new(inner.x, inner.y + 48.0, inner.w, 38.0), &mut name, "", false);
            let on_server = app.server.is_connected();
            let by = inner.bottom() - 36.0;
            let mut right = inner.right();
            let save_local = tool_button_right(ui, &mut right, by, id("dlg-save-local"), app.tr("Save on this device"), None, !on_server);
            let save_server = on_server && tool_button_right(ui, &mut right, by, id("dlg-save-server"), app.tr("Save on server"), None, true);
            let cancel = tool_button_right(ui, &mut right, by, id("dlg-cancel"), app.tr("Cancel"), None, false);
            if (save_local || save_server || submit) && !name.trim().is_empty() {
                let srv = save_server || (submit && on_server);
                app.save_preset_as(host, name.trim(), srv);
                true
            } else {
                if let Some(Dialog::SavePresetAs(n)) = &mut app.view.dialog {
                    *n = name;
                }
                cancel
            }
        }
        Dialog::Confirm { title, message, action } => {
            ui.text(Rect::new(inner.x, inner.y, inner.w, 28.0), &title, 18.0, Weight::Bold, t.text, Align::Left);
            ui.paragraph(Rect::new(inner.x, inner.y + 44.0, inner.w, 80.0), &message, 14.0, Weight::Regular, t.text_dim);
            let (ok, cancel) = buttons(app, ui, inner);
            if ok {
                run_confirm(app, host, action);
            }
            ok || cancel
        }
        Dialog::FileBrowser(fb) => file_browser(app, ui, host, inner, fb),
        Dialog::AddToPlaylist { items, kind } => add_to_playlist(app, ui, host, inner, items, kind),
        Dialog::Login(_) | Dialog::About => true,
    };
    if close {
        app.view.dialog = None;
    }
    ui.set_layer(0);
}

fn buttons(app: &App, ui: &mut Ui, inner: Rect) -> (bool, bool) {
    let by = inner.bottom() - 36.0;
    let mut right = inner.right();
    let ok = tool_button_right(ui, &mut right, by, id("dlg-ok"), app.tr("OK"), None, true);
    let cancel = tool_button_right(ui, &mut right, by, id("dlg-cancel"), app.tr("Cancel"), None, false);
    (ok, cancel)
}

fn run_prompt(app: &mut App, host: &mut dyn Host, action: PromptAction, value: String) {
    match action {
        PromptAction::NewPlaylist { kind, items } => {
            let on_server = app.server.is_connected();
            app.create_playlist(host, &value, kind, items, on_server);
        }
        PromptAction::RenamePlaylist(pid) => {
            let pl = app.local_playlists.iter().chain(app.server_playlists.iter()).find(|p| p.id == pid).cloned();
            if let Some(mut p) = pl {
                p.name = value;
                app.update_playlist(host, &p);
            }
        }
        PromptAction::RenameEffect(eid) => {
            let before = app.preset.clone();
            if let Some(e) = app.preset.find_effect_mut(&eid) {
                e.name = value;
            }
            app.preset_edited(before);
        }
    }
}

fn run_confirm(app: &mut App, host: &mut dyn Host, action: ConfirmAction) {
    match action {
        ConfirmAction::DeletePlaylist(id) => app.delete_playlist(host, &id),
        ConfirmAction::DeletePreset(name) => {
            if let Some(e) = app.preset_list.iter().find(|p| p.name == name && p.origin == app.preset_origin).cloned() {
                app.delete_preset(host, &e);
                app.load_preset_by_name(host, "Balanced");
            }
        }
        ConfirmAction::ClearQueue => {
            app.player.stop(host);
            app.queue.clear();
            app.current = None;
            app.settings_changed();
        }
        ConfirmAction::ClearBackgrounds => {
            app.bg_queue.clear();
            app.show_queue_background(host);
            app.settings_changed();
        }
        ConfirmAction::RemoveLibraryFolder(f) => {
            app.settings.library_folders.retain(|x| *x != f);
            let prefix = format!("{LOCAL_PREFIX}{f}");
            let remaining: Vec<_> = app.library.local.iter().filter(|t| !t.id.starts_with(&prefix)).cloned().collect();
            app.library.set_local(remaining);
            host.storage_write("library-local.json", app.library.local_cache_json().as_bytes());
            app.settings_changed();
        }
    }
}

fn add_to_playlist(app: &mut App, ui: &mut Ui, host: &mut dyn Host, inner: Rect, items: Vec<String>, kind: PlaylistKind) -> bool {
    let t = ui.theme().clone();
    ui.text(Rect::new(inner.x, inner.y, inner.w, 28.0), app.tr("Add to playlist"), 18.0, Weight::Bold, t.text, Align::Left);
    let lists: Vec<bl_net::api::Playlist> = app.local_playlists.iter().chain(app.server_playlists.iter()).filter(|p| p.kind == kind).cloned().collect();
    let list_r = Rect::new(inner.x, inner.y + 44.0, inner.w, inner.h - 44.0 - 50.0);
    let sid = id("dlg-pl-list");
    let off = ui.begin_scroll(sid, list_r, lists.len() as f32 * 48.0);
    let mut chosen = None;
    for (i, pl) in lists.iter().enumerate() {
        let rr = Rect::new(list_r.x, list_r.y + i as f32 * 48.0 - off, list_r.w - 8.0, 44.0);
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        row_background(ui, rr, resp.hovered, false, i);
        ui.icon(Rect::new(rr.x + 6.0, rr.y, 28.0, rr.h), Icon::Playlist, 18.0, t.primary_hover);
        ui.text(Rect::new(rr.x + 44.0, rr.y, rr.w - 50.0, rr.h), &pl.name, 14.0, Weight::Medium, t.text, Align::Left);
        if resp.clicked {
            chosen = Some(pl.clone());
        }
    }
    ui.end_scroll(sid, list_r);
    let by = inner.bottom() - 36.0;
    let mut right = inner.right();
    let cancel = tool_button_right(ui, &mut right, by, id("dlg-cancel"), app.tr("Cancel"), None, false);
    let new = tool_button_right(ui, &mut right, by, id("dlg-new-pl"), app.tr("New playlist…"), Some(Icon::Plus), false);
    if let Some(mut pl) = chosen {
        let n = items.len();
        pl.items.extend(items);
        app.update_playlist(host, &pl);
        app.toast(format!("{n} {} \"{}\"", app.tr("added to"), pl.name), false);
        return true;
    }
    if new {
        app.view.dialog = Some(Dialog::Prompt { title: app.tr("New playlist").into(), value: String::new(), action: PromptAction::NewPlaylist { kind, items } });
        return false;
    }
    cancel
}

fn accepts(purpose: BrowseFor, name: &str) -> bool {
    let ext = crate::jobs::extension(name).unwrap_or_default();
    match purpose {
        BrowseFor::AudioFiles => bl_media::is_supported_extension(&ext),
        BrowseFor::BackgroundFiles | BrowseFor::UploadImages => is_image_extension(&ext),
        BrowseFor::ImportPreset => ext == "hfx" || ext == "json",
        BrowseFor::LibraryFolder | BrowseFor::BackgroundFolder | BrowseFor::ExportPreset => false,
    }
}

fn file_browser(app: &mut App, ui: &mut Ui, host: &mut dyn Host, inner: Rect, mut fb: crate::view::FileBrowser) -> bool {
    let t = ui.theme().clone();
    let title = match fb.purpose {
        BrowseFor::LibraryFolder => app.tr("Choose a music folder"),
        BrowseFor::AudioFiles => app.tr("Open audio files"),
        BrowseFor::BackgroundFiles => app.tr("Choose background images"),
        BrowseFor::BackgroundFolder => app.tr("Choose an image folder"),
        BrowseFor::ImportPreset => app.tr("Import preset"),
        BrowseFor::ExportPreset => app.tr("Export preset to folder"),
        BrowseFor::UploadImages => app.tr("Upload images to the server"),
    };
    ui.text(Rect::new(inner.x, inner.y, inner.w, 28.0), title, 18.0, Weight::Bold, t.text, Align::Left);
    // Roots.
    let roots = host.fs_roots();
    let mut x = inner.x;
    let ry = inner.y + 36.0;
    for (i, (label, path)) in roots.iter().enumerate().take(8) {
        let w = ui.measure(label, 12.0, Weight::Medium) + 22.0;
        if x + w > inner.right() {
            break;
        }
        if ui.button(child(id("fb-root"), i as u64), Rect::new(x, ry, w, 28.0), label, None, false).clicked {
            fb.path = path.clone();
            fb.selected.clear();
        }
        x += w + 6.0;
    }
    // Path bar with "up".
    let py = ry + 36.0;
    if ui.icon_button(id("fb-up"), Rect::new(inner.x - 4.0, py, 32.0, 32.0), Icon::ChevronUp, 18.0, false, app.tr("Up")).clicked {
        if let Some(p) = std::path::Path::new(&fb.path).parent() {
            fb.path = p.to_string_lossy().into_owned();
            fb.selected.clear();
        }
    }
    ui.text(Rect::new(inner.x + 34.0, py, inner.w - 34.0, 32.0), &fb.path, 13.0, Weight::Medium, t.text_dim, Align::Left);
    // Listing.
    let list_r = Rect::new(inner.x, py + 40.0, inner.w, inner.bottom() - py - 40.0 - 48.0);
    let entries = host.fs_list(&fb.path).unwrap_or_default();
    let shown: Vec<_> = entries.into_iter().filter(|e| e.is_dir || accepts(fb.purpose, &e.name)).collect();
    let row_h = 40.0;
    let sid = child(id("fb-list"), bl_ui::id(&fb.path));
    let off = ui.begin_scroll(sid, list_r, shown.len() as f32 * row_h);
    let mut enter = None;
    for (i, e) in shown.iter().enumerate() {
        let rr = Rect::new(list_r.x, list_r.y + i as f32 * row_h - off, list_r.w - 8.0, row_h - 2.0);
        if rr.bottom() < list_r.y || rr.y > list_r.bottom() {
            continue;
        }
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        let selected = fb.selected.contains(&e.path);
        row_background(ui, rr, resp.hovered, selected, i);
        ui.icon(Rect::new(rr.x + 6.0, rr.y, 26.0, rr.h), if e.is_dir { Icon::Folder } else { Icon::File }, 18.0, if e.is_dir { t.primary_hover } else { t.text_dim });
        ui.text(Rect::new(rr.x + 40.0, rr.y, rr.w - 50.0, rr.h), &e.name, 13.0, Weight::Regular, t.text, Align::Left);
        if resp.clicked && !ui.is_drag_scrolling() {
            if e.is_dir {
                enter = Some(e.path.clone());
            } else if selected {
                fb.selected.retain(|p| *p != e.path);
            } else {
                if fb.purpose == BrowseFor::ImportPreset {
                    fb.selected.clear();
                }
                fb.selected.push(e.path.clone());
            }
        }
    }
    ui.end_scroll(sid, list_r);
    if let Some(p) = enter {
        fb.path = p;
        fb.selected.clear();
    }
    // Actions.
    let by = inner.bottom() - 36.0;
    let mut right = inner.right();
    let folder_mode = matches!(fb.purpose, BrowseFor::LibraryFolder | BrowseFor::BackgroundFolder | BrowseFor::ExportPreset);
    let ok_label = match fb.purpose {
        BrowseFor::LibraryFolder | BrowseFor::BackgroundFolder => app.tr("Use this folder"),
        BrowseFor::ExportPreset => app.tr("Export here"),
        BrowseFor::ImportPreset => app.tr("Import"),
        BrowseFor::UploadImages => app.tr("Upload"),
        _ => app.tr("Add"),
    };
    let ok = tool_button_right(ui, &mut right, by, id("fb-ok"), ok_label, None, true);
    let cancel = tool_button_right(ui, &mut right, by, id("fb-cancel"), app.tr("Cancel"), None, false);
    if !folder_mode && !fb.selected.is_empty() {
        let n = format!("{} {}", fb.selected.len(), app.tr("selected"));
        ui.text(Rect::new(inner.x, by, 200.0, 34.0), &n, 12.0, Weight::Medium, t.text_dim, Align::Left);
    }
    if ok && (folder_mode || !fb.selected.is_empty()) {
        finish_browse(app, host, &fb);
        return true;
    }
    if let Some(Dialog::FileBrowser(cur)) = &mut app.view.dialog {
        *cur = fb;
    }
    let _ = Color::WHITE;
    cancel
}

fn finish_browse(app: &mut App, host: &mut dyn Host, fb: &crate::view::FileBrowser) {
    match fb.purpose {
        BrowseFor::LibraryFolder => {
            if !app.settings.library_folders.contains(&fb.path) {
                app.settings.library_folders.push(fb.path.clone());
                app.settings_changed();
            }
            app.rescan_local();
        }
        BrowseFor::AudioFiles => {
            let ids: Vec<String> = fb.selected.iter().map(|p| format!("{LOCAL_PREFIX}{p}")).collect();
            app.enqueue(host, ids);
        }
        BrowseFor::BackgroundFiles => {
            let ids: Vec<String> = fb.selected.iter().map(|p| format!("{LOCAL_PREFIX}{p}")).collect();
            app.add_backgrounds(host, ids, true);
        }
        BrowseFor::BackgroundFolder => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ids: Vec<String> = crate::library::list_images(&fb.path).into_iter().map(|p| format!("{LOCAL_PREFIX}{p}")).collect();
                let n = ids.len();
                app.add_backgrounds(host, ids, true);
                app.toast(format!("{n} {}", app.tr("images added")), false);
            }
        }
        BrowseFor::ImportPreset => {
            if let Some(p) = fb.selected.first() {
                if let Ok(d) = host.fs_read(p) {
                    let name = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
                    app.import_preset_bytes(&name, &d);
                }
            }
        }
        BrowseFor::ExportPreset => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = std::path::Path::new(&fb.path).join(format!("{}.hfx", app.preset.name));
                match std::fs::write(&path, app.preset.to_string_pretty()) {
                    Ok(()) => app.toast(format!("{} {}", app.tr("Exported to"), path.display()), false),
                    Err(e) => app.toast(e.to_string(), true),
                }
            }
        }
        BrowseFor::UploadImages => {
            for p in &fb.selected {
                if let Ok(d) = host.fs_read(p) {
                    let name = std::path::Path::new(p).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    app.upload_image(host, &name, d);
                }
            }
        }
    }
}
