//! Backgrounds page: images from the server, local folders and files.

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::view::{BrowseFor, Dialog, FileBrowser};
use oc_ui::{child, id, Align, Color, Icon, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let body = page_header(ui, r, app.tr("Backgrounds"), app.tr("Images the visualizer animates. Click an image to show it, use + to add it to the background queue."));
    let (bar, rest) = body.split_top(46.0);
    let mut x = bar.x;
    if host.has_filesystem() {
        if tool_button(ui, &mut x, bar.y, id("bg-add-files"), app.tr("Add images…"), Some(Icon::Plus), true) {
            app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::BackgroundFiles, path: pictures_dir(host), selected: Vec::new() }));
        }
        if tool_button(ui, &mut x, bar.y, id("bg-add-folder"), app.tr("Add folder…"), Some(Icon::Folder), false) {
            app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::BackgroundFolder, path: pictures_dir(host), selected: Vec::new() }));
        }
    } else if tool_button(ui, &mut x, bar.y, id("bg-open"), app.tr("Open images…"), Some(Icon::Plus), true) {
        host.pick_files("image/*", true);
    }
    if app.server.is_connected() && tool_button(ui, &mut x, bar.y, id("bg-upload"), app.tr("Upload to server…"), Some(Icon::Upload), false) {
        if host.has_filesystem() {
            app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::UploadImages, path: pictures_dir(host), selected: Vec::new() }));
        } else {
            app.view.upload_next_drop = true;
            host.pick_files("image/*", true);
        }
    }
    let mut cover = app.settings.cover_fallback;
    let cr = Rect::new(bar.right() - 260.0, bar.y, 260.0, 34.0);
    if ui.checkbox(id("bg-cover"), cr, &mut cover, app.tr("Album cover as fallback")) {
        app.settings.cover_fallback = cover;
        app.settings_changed();
        app.show_queue_background(host);
    }
    if app.server_images.is_empty() {
        let msg = if app.server.is_connected() {
            app.tr("The server has no images yet. Configure an image folder on the server or upload images.")
        } else {
            app.tr("Images you add are listed in the background queue (Queue → Backgrounds). Connect to a server to browse its image library.")
        };
        empty_state(ui, rest, Icon::Image, app.tr("Server images"), msg);
        return;
    }
    // Grid of server images (thumbnails via the image endpoint).
    let images = app.server_images.clone();
    let cell = if rest.w < 520.0 { (rest.w - 26.0) / 2.0 } else { 200.0 };
    let cols = (((rest.w - 10.0) / (cell + 12.0)).floor() as usize).max(1);
    let cw = (rest.w - 10.0 - (cols - 1) as f32 * 12.0) / cols as f32;
    let ch = cw * 9.0 / 16.0 + 28.0;
    let rows = images.len().div_ceil(cols);
    let sid = id("bg-grid");
    let off = ui.begin_scroll(sid, rest, rows as f32 * (ch + 12.0));
    let mut show = None;
    let mut add = None;
    let current = app.bg_source.clone();
    for (i, img) in images.iter().enumerate() {
        let (row, col) = (i / cols, i % cols);
        let cr = Rect::new(rest.x + col as f32 * (cw + 12.0), rest.y + row as f32 * (ch + 12.0) - off, cw, ch);
        if cr.bottom() < rest.y || cr.y > rest.bottom() {
            continue;
        }
        let resp = ui.interact(child(sid, i as u64 + 1), cr);
        let thumb = Rect::new(cr.x, cr.y, cw, cw * 9.0 / 16.0);
        let key = format!("img:{}", img.id);
        if let Some((tex, w, h)) = app.cover_texture(&key) {
            ui.image_cover(thumb, tex, w, h, Color::WHITE, 8.0);
        } else {
            if !app.covers.contains_key(&key) {
                app.request_image_thumb(host, &key, &img.id);
            }
            ui.rect(thumb, Color(1.0, 1.0, 1.0, 0.07), 8.0);
            ui.icon(thumb, Icon::Image, 28.0, t.text_faint);
        }
        let source = format!("srv:{}", img.id);
        if current.as_deref() == Some(source.as_str()) {
            ui.outline(thumb, t.primary, 8.0, 2.0);
        } else if resp.hovered {
            ui.outline(thumb, Color(1.0, 1.0, 1.0, 0.5), 8.0, 1.5);
        }
        ui.text(Rect::new(cr.x, thumb.bottom() + 4.0, cw - 34.0, 22.0), &img.name, 12.0, Weight::Regular, t.text_dim, Align::Left);
        if ui.icon_button(child(sid, 800_000 + i as u64), Rect::new(cr.right() - 30.0, thumb.bottom() + 1.0, 28.0, 26.0), Icon::Plus, 16.0, false, app.tr("Add to background queue")).clicked {
            add = Some(source.clone());
        } else if resp.clicked && !ui.is_drag_scrolling() {
            show = Some(source);
        }
    }
    ui.end_scroll(sid, rest);
    if let Some(s) = show {
        app.add_backgrounds(host, vec![s], true);
    }
    if let Some(s) = add {
        app.add_backgrounds(host, vec![s], false);
        app.toast(app.tr("Added to the background queue"), false);
    }
}

fn pictures_dir(host: &dyn Host) -> String {
    let roots = host.fs_roots();
    roots.iter().find(|(l, _)| l == "Pictures").or(roots.first()).map(|(_, p)| p.clone()).unwrap_or_else(|| "/".into())
}
