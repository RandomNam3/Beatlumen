//! Settings page.

use super::common::*;
use crate::app::App;
use crate::host::Host;
use crate::server::ServerState;
use crate::settings::{OverlayVisibility, VisualizerKind};
use crate::view::{BrowseFor, ConfirmAction, Dialog, FileBrowser, SettingsTab};
use oc_ui::{child, id, Align, Color, Icon, Rect, RectExt, Theme, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let body = page_header(ui, r, app.tr("Settings"), "");
    let (bar, rest) = body.split_top(48.0);
    let tabs = [SettingsTab::General, SettingsTab::Overlay, SettingsTab::Server, SettingsTab::Library, SettingsTab::About];
    let labels = [app.tr("General"), app.tr("Overlay"), app.tr("Server"), app.tr("Library"), app.tr("About")];
    let mut i = tabs.iter().position(|t| *t == app.view.settings_tab).unwrap_or(0);
    if ui.tabs(id("settings-tabs"), Rect::new(bar.x, bar.y, bar.w.min(620.0), 36.0), &mut i, &labels) {
        app.view.settings_tab = tabs[i];
    }
    let content = Rect::new(rest.x, rest.y, rest.w.min(760.0), rest.h);
    match app.view.settings_tab {
        SettingsTab::General => general(app, ui, host, content),
        SettingsTab::Overlay => overlay(app, ui, content),
        SettingsTab::Server => server(app, ui, host, content),
        SettingsTab::Library => library(app, ui, host, content),
        SettingsTab::About => about(app, ui, content),
    }
}

fn general(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let row = |i: usize| Rect::new(r.x, r.y + i as f32 * 54.0, r.w, 50.0);
    // Language.
    let c = field_row(ui, row(0), app.tr("Language"), "");
    let langs = vec!["English".to_string(), "Deutsch".to_string()];
    let mut li = if app.settings.language.starts_with("de") { 1 } else { 0 };
    if ui.select(id("set-lang"), c, &mut li, &langs) {
        app.settings.language = if li == 1 { "de".into() } else { "en".into() };
        app.settings_changed();
    }
    // UI zoom.
    let c = field_row(ui, row(1), app.tr("Interface size"), "");
    let mut z = app.settings.ui_zoom;
    if ui.slider(id("set-zoom"), Rect::new(c.x, c.y, c.w - 70.0, c.h), &mut z, 0.5, 2.0, 0.05) {
        app.settings.ui_zoom = z;
        app.settings_changed();
    }
    let zl = format!("{:.0} %", app.settings.ui_zoom * 100.0);
    ui.text(Rect::new(c.right() - 60.0, c.y, 60.0, c.h), &zl, 13.0, Weight::Medium, ui.theme().text, Align::Right);
    // Accent colour.
    let c = field_row(ui, row(2), app.tr("Accent colour"), "");
    for (i, (name, hue)) in Theme::accent_presets().iter().enumerate() {
        let cr = Rect::new(c.x + i as f32 * 40.0, c.y + 2.0, 30.0, 30.0);
        let resp = ui.interact(child(id("accent"), i as u64), cr);
        let color = Theme::dark(*hue).primary;
        ui.rect(cr, color, 15.0);
        if (app.settings.accent_hue - hue).abs() < 0.5 {
            ui.outline(cr.shrink(-3.0), Color::WHITE, 18.0, 2.0);
        }
        ui.tooltip(child(id("accent"), i as u64), cr, app.tr(name));
        if resp.clicked {
            app.settings.accent_hue = *hue;
            app.settings_changed();
        }
    }
    // Render scale.
    let c = field_row(ui, row(3), app.tr("Visualizer resolution"), app.tr("Lower values improve performance"));
    let mut s = app.settings.render_scale;
    if ui.slider(id("set-rs"), Rect::new(c.x, c.y, c.w - 70.0, c.h), &mut s, 0.25, 1.0, 0.05) {
        app.settings.render_scale = s;
        app.settings_changed();
    }
    let sl = format!("{:.0} %", app.settings.render_scale * 100.0);
    ui.text(Rect::new(c.right() - 60.0, c.y, 60.0, c.h), &sl, 13.0, Weight::Medium, ui.theme().text, Align::Right);
    // FPS.
    let c = field_row(ui, row(4), app.tr("Show frame rate"), "");
    let mut fps = app.settings.show_fps;
    if ui.toggle(id("set-fps"), Rect::new(c.x, c.y, 60.0, c.h), &mut fps) {
        app.settings.show_fps = fps;
        app.settings_changed();
    }
    // Fullscreen.
    let c = field_row(ui, row(5), app.tr("Fullscreen"), "");
    let mut fs = host.is_fullscreen();
    if ui.toggle(id("set-fs"), Rect::new(c.x, c.y, 60.0, c.h), &mut fs) {
        host.set_fullscreen(fs);
    }
}

fn overlay(app: &mut App, ui: &mut Ui, r: Rect) {
    let row = |i: usize| Rect::new(r.x, r.y + i as f32 * 54.0, r.w, 50.0);
    let c = field_row(ui, row(0), app.tr("Now playing overlay"), app.tr("Title and visualizer over the background"));
    let opts: Vec<String> = OverlayVisibility::ALL
        .iter()
        .map(|v| {
            app.tr(match v {
                OverlayVisibility::Hide => "Never",
                OverlayVisibility::HideOnMenu => "When controls are hidden",
                OverlayVisibility::AlwaysVisible => "Always",
            })
            .to_string()
        })
        .collect();
    let mut i = OverlayVisibility::ALL.iter().position(|v| *v == app.settings.overlay_visibility).unwrap_or(1);
    if ui.select(id("set-ov"), c, &mut i, &opts) {
        app.settings.overlay_visibility = OverlayVisibility::ALL[i];
        app.settings_changed();
    }
    let c = field_row(ui, row(1), app.tr("Visualizer"), "");
    let opts: Vec<String> = VisualizerKind::ALL.iter().map(|v| app.tr(v.name()).to_string()).collect();
    let mut i = VisualizerKind::ALL.iter().position(|v| *v == app.settings.visualizer).unwrap_or(1);
    if ui.select(id("set-vis"), c, &mut i, &opts) {
        app.settings.visualizer = VisualizerKind::ALL[i];
        app.settings_changed();
    }
    let sliders: [(&str, u64, f32, f32, f32); 6] = [
        ("Spectrum minimum frequency (Hz)", 1, 0.0, 4000.0, 10.0),
        ("Spectrum maximum frequency (Hz)", 2, 0.0, 4000.0, 10.0),
        ("Spectrum smoothing", 3, 0.0, 0.99, 0.01),
        ("Bar width", 4, 1.0, 12.0, 1.0),
        ("Bar gap", 5, 0.0, 16.0, 1.0),
        ("Waveform thickness", 6, 0.0, 0.99, 0.01),
    ];
    for (k, (label, key, min, max, step)) in sliders.iter().enumerate() {
        let c = field_row(ui, row(2 + k), app.tr(label), "");
        let v = match key {
            1 => &mut app.settings.spectrum_min_freq,
            2 => &mut app.settings.spectrum_max_freq,
            3 => &mut app.settings.spectrum_smoothing,
            4 => &mut app.settings.bar_width,
            5 => &mut app.settings.bar_gap,
            _ => &mut app.settings.waveform_thickness,
        };
        let changed = ui.slider(child(id("set-vis-s"), *key), Rect::new(c.x, c.y, c.w - 70.0, c.h), v, *min, *max, *step);
        let label = if *step >= 1.0 { format!("{:.0}", v) } else { format!("{:.2}", v) };
        ui.text(Rect::new(c.right() - 60.0, c.y, 60.0, c.h), &label, 13.0, Weight::Medium, ui.theme().text, Align::Right);
        if changed {
            app.settings_changed();
        }
    }
}

fn server(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let row = |i: usize| Rect::new(r.x, r.y + i as f32 * 54.0, r.w, 50.0);
    let web = host.platform() == "web";
    if web {
        ui.paragraph(Rect::new(r.x, r.y, r.w, 60.0), app.tr("The web player always uses the server it was loaded from."), 13.0, Weight::Regular, t.text_dim);
    } else {
        if app.view.server_url_edit.is_empty() {
            app.view.server_url_edit = app.server.base.clone();
        }
        let c = field_row(ui, row(0), app.tr("Server address"), app.tr("e.g. http://192.168.1.10:7878"));
        let (urlr, btnr) = c.split_right(130.0);
        let (_, submit) = ui.text_input(id("srv-url"), Rect::new(urlr.x, urlr.y, urlr.w - 8.0, urlr.h), &mut app.view.server_url_edit, "http://host:7878", false);
        if ui.button(id("srv-connect"), btnr, app.tr("Connect"), Some(Icon::Server), true).clicked || submit {
            let url = app.view.server_url_edit.trim().trim_end_matches('/').to_string();
            if url.starts_with("https://") {
                app.toast(app.tr("HTTPS is not built in; use http:// in your LAN/VPN or a TLS-terminating proxy on the same host."), true);
            }
            app.settings.server_url = url.clone();
            app.settings.use_server = !url.is_empty();
            if app.server.base != url {
                app.settings.server_token.clear();
                app.server.token.clear();
            }
            app.server.base = url;
            app.settings_changed();
            app.connect_server(host);
        }
        let c = field_row(ui, row(1), app.tr("Use server"), "");
        let mut use_server = app.settings.use_server;
        if ui.toggle(id("srv-use"), Rect::new(c.x, c.y, 60.0, c.h), &mut use_server) {
            app.settings.use_server = use_server;
            app.settings_changed();
            if use_server {
                app.connect_server(host);
            } else {
                app.server.state = ServerState::Disabled;
                app.library.set_remote(Default::default());
                app.server_playlists.clear();
                app.server_images.clear();
            }
        }
    }
    let y0 = if web { r.y + 70.0 } else { r.y + 120.0 };
    let status_r = Rect::new(r.x, y0, r.w, 80.0);
    ui.rect(status_r, t.surface, 10.0);
    let (title, detail, color) = match &app.server.state {
        ServerState::Disabled => (app.tr("Not connected").to_string(), app.tr("Only local files are used.").to_string(), t.text_faint),
        ServerState::Connecting => (app.tr("Connecting…").to_string(), String::new(), t.text_dim),
        ServerState::Connected(i) => (
            format!("{} {}", app.tr("Connected to"), i.name),
            format!("OpenChroma Server {} · {} {}", i.version, i.track_count, app.tr("tracks")),
            t.success,
        ),
        ServerState::NeedsLogin(i) => (format!("{} – {}", i.name, app.tr("login required")), String::new(), t.warning),
        ServerState::Error(e) => (app.tr("Connection failed").to_string(), e.clone(), t.danger),
    };
    ui.rect(Rect::new(status_r.x + 16.0, status_r.y + 20.0, 10.0, 10.0), color, 5.0);
    ui.text(Rect::new(status_r.x + 36.0, status_r.y + 12.0, status_r.w - 52.0, 24.0), &title, 15.0, Weight::SemiBold, t.text, Align::Left);
    ui.text(Rect::new(status_r.x + 36.0, status_r.y + 40.0, status_r.w - 52.0, 20.0), &detail, 12.0, Weight::Regular, t.text_dim, Align::Left);
    let mut y = status_r.bottom() + 16.0;
    if matches!(app.server.state, ServerState::NeedsLogin(_)) {
        let c = field_row(ui, Rect::new(r.x, y, r.w, 50.0), app.tr("Password"), "");
        let (pr, br) = c.split_right(130.0);
        let (_, submit) = ui.text_input(id("srv-pass"), Rect::new(pr.x, pr.y, pr.w - 8.0, pr.h), &mut app.view.server_password, "", true);
        if ui.button(id("srv-login"), br, app.tr("Log in"), None, true).clicked || submit {
            let pw = std::mem::take(&mut app.view.server_password);
            app.server_login(host, &pw);
        }
        y += 60.0;
    }
    if app.server.is_connected() {
        let mut x = r.x;
        if tool_button(ui, &mut x, y, id("srv-refresh"), app.tr("Reload library"), Some(Icon::Refresh), false) {
            app.refresh_server_data(host);
        }
        if tool_button(ui, &mut x, y, id("srv-rescan"), app.tr("Rescan server folders"), Some(Icon::Server), false) {
            app.server_rescan(host);
        }
        if !app.server.token.is_empty() && tool_button(ui, &mut x, y, id("srv-logout"), app.tr("Log out"), Some(Icon::Logout), false) {
            app.server.token.clear();
            app.settings.server_token.clear();
            app.settings_changed();
            app.connect_server(host);
        }
    }
}

fn library(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    if !host.has_filesystem() {
        ui.paragraph(Rect::new(r.x, r.y, r.w, 60.0), app.tr("Local folders are not available in the browser. Your music comes from the server."), 13.0, Weight::Regular, t.text_dim);
        return;
    }
    let mut x = r.x;
    if tool_button(ui, &mut x, r.y, id("lib-add-folder"), app.tr("Add music folder…"), Some(Icon::Plus), true) {
        let start = host.fs_roots().iter().find(|(l, _)| l == "Music").or(host.fs_roots().first()).map(|(_, p)| p.clone()).unwrap_or_default();
        app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::LibraryFolder, path: start, selected: Vec::new() }));
    }
    if tool_button(ui, &mut x, r.y, id("lib-rescan"), app.tr("Rescan"), Some(Icon::Refresh), false) {
        app.rescan_local();
    }
    if !host.has_media_permission() && tool_button(ui, &mut x, r.y, id("lib-perm"), app.tr("Allow access to music"), Some(Icon::Warning), false) {
        host.request_media_permission();
    }
    let folders = app.settings.library_folders.clone();
    if folders.is_empty() {
        empty_state(ui, Rect::new(r.x, r.y + 50.0, r.w, r.h - 50.0), Icon::Folder, app.tr("No music folders"), app.tr("Add folders that contain your music. They are scanned in the background."));
        return;
    }
    for (i, f) in folders.iter().enumerate() {
        let rr = Rect::new(r.x, r.y + 52.0 + i as f32 * 50.0, r.w, 46.0);
        row_background(ui, rr, false, false, i);
        ui.icon(Rect::new(rr.x + 8.0, rr.y, 28.0, rr.h), Icon::Folder, 20.0, t.primary_hover);
        ui.text(Rect::new(rr.x + 46.0, rr.y, rr.w - 100.0, rr.h), f, 14.0, Weight::Regular, t.text, Align::Left);
        if ui.icon_button(child(id("lib-folder-rm"), i as u64), Rect::new(rr.right() - 44.0, rr.y + 5.0, 36.0, 36.0), Icon::Close, 16.0, false, app.tr("Remove")).clicked {
            app.view.dialog = Some(Dialog::Confirm {
                title: app.tr("Remove folder").into(),
                message: format!("{} {f}", app.tr("Remove from the library:")),
                action: ConfirmAction::RemoveLibraryFolder(f.clone()),
            });
        }
    }
}

fn about(app: &mut App, ui: &mut Ui, r: Rect) {
    let t = ui.theme().clone();
    let mut y = r.y;
    ui.text(Rect::new(r.x, y, r.w, 30.0), &format!("OpenChroma {}", env!("CARGO_PKG_VERSION")), 20.0, Weight::Bold, t.text, Align::Left);
    y += 40.0;
    let text = app.tr(
        "An open source music player that turns your music and images into real-time music videos. \
         A clean-room rewrite inspired by Hyperchroma, written in Rust without external libraries: own decoders \
         (MP3, AAC, Opus, Vorbis, FLAC, ALAC, WAV/AIFF), image codecs, font rendering, UI and OpenGL renderer.",
    );
    y += ui.paragraph(Rect::new(r.x, y, r.w, 200.0), text, 13.0, Weight::Regular, t.text_dim) + 16.0;
    let licenses = [
        "OpenChroma: GNU GPL v3.0 or later",
        "Inter font: SIL Open Font License 1.1 (The Inter Project Authors)",
        "Effect shaders ported from midori (MIT, Benjamin Pang) and three.js (MIT)",
        "Simplex noise: webgl-noise (MIT, Ashima Arts / Stefan Gustavson)",
        "MP3 decoder core ported from minimp3 (CC0)",
        "Opus decoder ported from libopus (BSD-3-Clause, Xiph.Org and others)",
        "Vorbis floor/codebook details after libvorbis (BSD-3-Clause, Xiph.Org)",
        "Glyph rasterizer after font-rs (Apache-2.0, Google)",
    ];
    section_title(ui, Rect::new(r.x, y, r.w, 20.0), &app.tr("Licenses").to_uppercase());
    y += 26.0;
    for l in licenses {
        ui.text(Rect::new(r.x, y, r.w, 22.0), l, 13.0, Weight::Regular, t.text, Align::Left);
        y += 24.0;
    }
    y += 12.0;
    section_title(ui, Rect::new(r.x, y, r.w, 20.0), &app.tr("Keyboard shortcuts").to_uppercase());
    y += 26.0;
    let keys = [
        ("Esc", "Show / hide menus"),
        ("Space", "Play / pause"),
        ("Ctrl + ← / →", "Previous / next track"),
        ("← / →", "Seek 5 seconds"),
        ("Ctrl + ↑ / ↓", "Volume"),
        ("Ctrl + Alt + ← / →", "Previous / next background"),
        ("Alt + Enter / F11", "Fullscreen"),
        ("Ctrl + Alt + Enter", "Theater mode"),
        ("Alt + P", "Pin overlay"),
        ("Ctrl + F", "Search library"),
        ("Alt + S", "Save preset"),
        ("Ctrl + Z / Y", "Undo / redo preset changes"),
    ];
    for (k, d) in keys {
        ui.text(Rect::new(r.x, y, 200.0, 22.0), k, 13.0, Weight::SemiBold, t.text, Align::Left);
        ui.text(Rect::new(r.x + 210.0, y, r.w - 210.0, 22.0), app.tr(d), 13.0, Weight::Regular, t.text_dim, Align::Left);
        y += 24.0;
    }
}
