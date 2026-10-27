//! All screens of the application.

mod backgrounds;
mod common;
mod dialogs;
mod effects;
mod library;
mod playlists;
mod queue;
mod settings;
mod visual;

use crate::app::App;
use crate::host::Host;
use crate::player::PlayState;
use crate::settings::{OverlayVisibility, Repeat};
use crate::view::Page;
use common::*;
use oc_ui::{child, id, Align, Color, Icon, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host) {
    let screen = ui.screen();
    let hidden = app.chrome_hidden();
    let chrome = ui.anim(id("chrome"), if hidden { 0.0 } else { 1.0 }, 8.0);
    let menu = ui.anim(id("menu-open"), if app.view.menu_open { 1.0 } else { 0.0 }, 14.0);

    // "Now playing" overlay.
    let show_now = match app.settings.overlay_visibility {
        OverlayVisibility::Hide => 0.0,
        OverlayVisibility::AlwaysVisible => 1.0 - menu,
        OverlayVisibility::HideOnMenu => (1.0 - chrome).min(1.0 - menu),
    };
    if show_now > 0.01 {
        now_playing(app, ui, host, show_now);
    }

    if chrome > 0.01 && menu < 0.99 {
        top_bar(app, ui, host, chrome * (1.0 - menu));
    }
    let bar_h = player_bar_height(screen);
    if chrome > 0.01 || menu > 0.01 {
        let a = chrome.max(menu);
        player_bar(app, ui, host, Rect::new(0.0, screen.h - bar_h, screen.w, bar_h), a);
    }
    if menu > 0.01 {
        menu_panel(app, ui, host, Rect::new(0.0, 0.0, screen.w, screen.h - bar_h), menu);
    }
    dialogs::draw(app, ui, host);
    toasts(app, ui, screen, bar_h);
    if app.settings.show_fps {
        let t = format!("{:.0} fps", app.fps);
        ui.text_at(8.0, 6.0, &t, 12.0, Weight::Medium, Color(1.0, 1.0, 1.0, 0.7));
    }
    if let Some(title) = app.download_progress.clone() {
        let r = Rect::new(screen.w / 2.0 - 160.0, 56.0, 320.0, 36.0);
        ui.rect(r, ui.theme().panel_strong, 18.0);
        let msg = format!("{} {title}", app.tr("Loading"));
        ui.text(r.pad(14.0, 0.0), &msg, 13.0, Weight::Medium, Color::WHITE, Align::Center);
    }
}

fn player_bar_height(screen: Rect) -> f32 {
    if screen.w < 560.0 {
        132.0
    } else {
        92.0
    }
}

fn top_bar(app: &mut App, ui: &mut Ui, host: &mut dyn Host, alpha: f32) {
    let screen = ui.screen();
    let r = Rect::new(0.0, 0.0, screen.w, 56.0);
    // Gradient shade.
    for i in 0..8 {
        let a = 0.32 * (1.0 - i as f32 / 8.0) * alpha;
        ui.rect(Rect::new(0.0, i as f32 * 9.0, screen.w, 9.0), Color(0.0, 0.0, 0.0, a), 0.0);
    }
    let white = Color(1.0, 1.0, 1.0, alpha);
    let (left, right) = r.pad(10.0, 8.0).split_right(200.0);
    if ui.icon_button(id("tb-menu"), Rect::new(left.x, left.y, 40.0, 40.0), Icon::Menu, 22.0, false, app.tr("Menu (Esc)")).clicked {
        app.view.menu_open = true;
    }
    ui.text(Rect::new(left.x + 50.0, left.y, 200.0, 40.0), "OpenChroma", 16.0, Weight::SemiBold, white, Align::Left);
    let mut x = right.right() - 40.0;
    let fs = host.is_fullscreen();
    if ui.icon_button(id("tb-fs"), Rect::new(x, right.y, 40.0, 40.0), if fs { Icon::FullscreenExit } else { Icon::Fullscreen }, 20.0, false, app.tr("Fullscreen")).clicked {
        host.set_fullscreen(!fs);
    }
    x -= 44.0;
    if ui.icon_button(id("tb-theater"), Rect::new(x, right.y, 40.0, 40.0), Icon::Theater, 20.0, app.view.theater, app.tr("Theater mode")).clicked {
        app.view.theater = !app.view.theater;
    }
    x -= 44.0;
    if ui.icon_button(id("tb-pin"), Rect::new(x, right.y, 40.0, 40.0), Icon::Pin, 20.0, app.settings.overlay_pinned, app.tr("Pin overlay")).clicked {
        app.settings.overlay_pinned = !app.settings.overlay_pinned;
        app.settings_changed();
    }
}

fn player_bar(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect, alpha: f32) {
    let t = ui.theme().clone();
    let compact = r.w < 560.0;
    // Background: dark gradient to the bottom.
    for i in 0..10 {
        let a = (0.12 + 0.6 * (i as f32 / 10.0)) * alpha;
        ui.rect(Rect::new(r.x, r.y + i as f32 * r.h / 10.0, r.w, r.h / 10.0 + 0.5), Color(0.02, 0.02, 0.03, a), 0.0);
    }
    ui.block(r);
    let pos = app.player.position(host);
    let dur = app.player.duration().or_else(|| app.current.as_ref().map(|c| c.duration_ms as f64 / 1000.0)).unwrap_or(0.0);
    // Seek bar.
    let seek_r = Rect::new(r.x + 16.0, r.y + 2.0, r.w - 32.0, 18.0);
    let mut v = if dur > 0.0 { (pos / dur) as f32 } else { 0.0 };
    let sid = id("seek");
    if ui.slider(sid, seek_r, &mut v, 0.0, 1.0, 0.0) && dur > 0.0 {
        app.view.seek_preview = Some(v as f64 * dur);
    }
    if let Some(target) = app.view.seek_preview {
        if !ui.input.down[0] {
            app.player.seek(host, target);
            app.view.seek_preview = None;
        }
    }
    let shown_pos = app.view.seek_preview.unwrap_or(pos);
    let time_y = r.y + 18.0;
    let tc = Color(1.0, 1.0, 1.0, 0.65 * alpha);
    ui.text(Rect::new(r.x + 16.0, time_y, 80.0, 16.0), &fmt_time(shown_pos), 11.0, Weight::Medium, tc, Align::Left);
    ui.text(Rect::new(r.right() - 96.0, time_y, 80.0, 16.0), &fmt_time(dur), 11.0, Weight::Medium, tc, Align::Right);

    let inner = Rect::new(r.x + 16.0, r.y + 34.0, r.w - 32.0, r.h - 42.0);
    let (info_r, controls_r, right_r) = if compact {
        let (top, bottom) = inner.split_top(44.0);
        let (c, rr) = bottom.split_right(110.0);
        (top, c, rr)
    } else {
        let side = (inner.w * 0.3).min(360.0);
        let (l, rest) = inner.split_left(side);
        let (c, rr) = rest.split_right(side);
        (l, c, rr)
    };
    // Track info with cover.
    if let Some(track) = app.current.clone() {
        let cover_r = Rect::new(info_r.x, info_r.y + (info_r.h - 44.0) / 2.0, 44.0, 44.0);
        draw_cover(app, ui, host, cover_r, &format!("track:{}", track.id), &track.id, 6.0, alpha);
        let tx = cover_r.right() + 12.0;
        let tw = info_r.right() - tx;
        let title_r = Rect::new(tx, info_r.y + info_r.h / 2.0 - 20.0, tw, 20.0);
        ui.text(title_r, &track.title, 14.0, Weight::SemiBold, Color(1.0, 1.0, 1.0, alpha), Align::Left);
        let sub = if track.album.is_empty() { track.artist_line() } else { format!("{} · {}", track.artist_line(), track.album) };
        ui.text(Rect::new(tx, title_r.bottom(), tw, 18.0), &sub, 12.0, Weight::Regular, Color(1.0, 1.0, 1.0, 0.65 * alpha), Align::Left);
        let click = ui.interact(id("bar-info"), info_r);
        if click.clicked {
            app.view.menu_open = true;
            app.view.page = Page::Queue;
        }
    } else {
        ui.text(info_r, app.tr("Nothing playing"), 13.0, Weight::Regular, Color(1.0, 1.0, 1.0, 0.5 * alpha), Align::Left);
    }
    // Transport controls.
    let cx = controls_r.x + controls_r.w / 2.0;
    let cy = controls_r.y + controls_r.h / 2.0;
    let playing = app.player.state() == PlayState::Playing;
    let loading = app.player.state() == PlayState::Loading;
    let play_r = Rect::new(cx - 24.0, cy - 24.0, 48.0, 48.0);
    let resp = ui.interact(id("play"), play_r);
    let hov = ui.anim(id("play"), if resp.hovered { 1.0 } else { 0.0 }, 16.0);
    ui.rect(play_r.shrink(-hov * 2.0), Color(1.0, 1.0, 1.0, alpha), 26.0);
    ui.icon(play_r, if playing || loading { Icon::Pause } else { Icon::Play }, 24.0, Color(0.06, 0.06, 0.08, alpha));
    if resp.clicked {
        app.toggle_play(host);
    }
    if resp.hovered {
        ui.ctx.wants_pointer_cursor = true;
    }
    let btn = |dx: f32| Rect::new(cx + dx - 20.0, cy - 20.0, 40.0, 40.0);
    if ui.icon_button(id("prev"), btn(-62.0), Icon::Prev, 22.0, false, app.tr("Previous")).clicked {
        app.prev_track(host);
    }
    if ui.icon_button(id("next"), btn(62.0), Icon::Next, 22.0, false, app.tr("Next")).clicked {
        app.next_track(host, true);
    }
    let shuffle = app.queue.shuffle;
    if ui.icon_button(id("shuffle"), btn(-114.0), Icon::Shuffle, 19.0, shuffle, app.tr("Shuffle")).clicked {
        app.queue.set_shuffle(!shuffle, &mut app.rng);
        app.settings_changed();
    }
    let rep = app.settings.repeat;
    let rep_icon = if rep == Repeat::Item { Icon::RepeatOne } else { Icon::Repeat };
    if ui.icon_button(id("repeat"), btn(114.0), rep_icon, 19.0, rep != Repeat::None, app.tr("Repeat")).clicked {
        app.settings.repeat = rep.next();
        app.settings_changed();
    }
    // Right side: background navigation, volume, queue.
    let mut x = right_r.right() - 40.0;
    let ry = right_r.y + (right_r.h - 40.0) / 2.0;
    if !compact {
        if ui.icon_button(id("bar-queue"), Rect::new(x, ry, 40.0, 40.0), Icon::Queue, 20.0, app.view.menu_open && app.view.page == Page::Queue, app.tr("Queue")).clicked {
            if app.view.menu_open && app.view.page == Page::Queue {
                app.view.menu_open = false;
            } else {
                app.view.menu_open = true;
                app.view.page = Page::Queue;
            }
        }
        x -= 44.0;
        if ui.icon_button(id("bg-next"), Rect::new(x, ry, 40.0, 40.0), Icon::Image, 19.0, false, app.tr("Next background")).clicked {
            app.next_background(host);
        }
        x -= 8.0;
        let vol_w = (right_r.w - 150.0).clamp(60.0, 120.0);
        x -= vol_w;
        let mut vol = if app.settings.muted { 0.0 } else { app.settings.volume };
        if ui.slider(id("volume"), Rect::new(x, ry + 10.0, vol_w, 20.0), &mut vol, 0.0, 1.0, 0.01) {
            app.set_volume(vol);
        }
        x -= 42.0;
    } else {
        x = right_r.x;
    }
    let vol_icon = if app.settings.muted || app.settings.volume == 0.0 {
        Icon::Mute
    } else if app.settings.volume < 0.5 {
        Icon::VolumeLow
    } else {
        Icon::Volume
    };
    if ui.icon_button(id("mute"), Rect::new(x, ry, 40.0, 40.0), vol_icon, 20.0, false, app.tr("Mute")).clicked {
        app.toggle_mute();
    }
    if compact {
        if ui.icon_button(id("bar-menu"), Rect::new(x + 50.0, ry, 40.0, 40.0), Icon::Menu, 20.0, app.view.menu_open, app.tr("Menu")).clicked {
            app.view.menu_open = !app.view.menu_open;
        }
    }
    let _ = (t, child(0, 0));
}

fn now_playing(app: &mut App, ui: &mut Ui, host: &mut dyn Host, alpha: f32) {
    let Some(track) = app.current.clone() else { return };
    let screen = ui.screen();
    let unit = (screen.h / 1080.0).clamp(0.45, 1.3);
    let margin = 56.0 * unit;
    let art = 168.0 * unit;
    let title_size = (54.0 * unit).max(20.0);
    let artist_size = (22.0 * unit).max(13.0);
    let bottom = screen.h - margin;
    let vis_h = if app.settings.visualizer == crate::settings::VisualizerKind::None { 0.0 } else { 64.0 * unit };
    let block_h = art.max(title_size * 1.3 + artist_size * 1.6);
    let y = bottom - block_h - vis_h - 12.0 * unit;
    let cover_r = Rect::new(margin, y + (block_h - art), art, art);
    ui.shadow(cover_r, Color(0.0, 0.0, 0.0, 0.45 * alpha), 24.0 * unit);
    draw_cover(app, ui, host, cover_r, &format!("track:{}", track.id), &track.id, 8.0 * unit, alpha);
    let tx = cover_r.right() + 28.0 * unit;
    let tw = screen.w - tx - margin;
    let pos_label = match app.queue.position() {
        Some(p) => format!("{:02}", p + 1),
        None => String::new(),
    };
    let ty = cover_r.bottom() - title_size * 1.3 - artist_size * 1.6;
    if !pos_label.is_empty() {
        ui.text(Rect::new(tx, ty - artist_size * 1.4, tw, artist_size * 1.4), &pos_label, artist_size * 0.8, Weight::SemiBold, Color(1.0, 1.0, 1.0, 0.6 * alpha), Align::Left);
    }
    let shadow = Color(0.0, 0.0, 0.0, 0.35 * alpha);
    ui.text(Rect::new(tx + 1.5, ty + 1.5, tw, title_size * 1.3), &track.title, title_size, Weight::Bold, shadow, Align::Left);
    ui.text(Rect::new(tx, ty, tw, title_size * 1.3), &track.title, title_size, Weight::Bold, Color(1.0, 1.0, 1.0, alpha), Align::Left);
    ui.text(Rect::new(tx, ty + title_size * 1.3, tw, artist_size * 1.6), &track.artist_line(), artist_size, Weight::Medium, Color(1.0, 1.0, 1.0, 0.82 * alpha), Align::Left);
    if vis_h > 0.0 {
        let vr = Rect::new(margin, bottom - vis_h, (screen.w - 2.0 * margin).min(900.0 * unit.max(0.8)), vis_h);
        visual::draw(app, ui, vr, alpha);
    }
}

fn menu_panel(app: &mut App, ui: &mut Ui, host: &mut dyn Host, area: Rect, alpha: f32) {
    let compact = area.w < 760.0;
    let margin = if compact { 0.0 } else { (area.w * 0.04).clamp(16.0, 48.0) };
    let slide = (1.0 - alpha) * 24.0;
    let panel = Rect::new(area.x + margin, area.y + margin.max(if compact { 0.0 } else { 16.0 }) + slide, area.w - 2.0 * margin, area.h - 2.0 * margin.max(if compact { 0.0 } else { 16.0 }));
    let t = ui.theme().clone();
    // Dim the background a little.
    ui.rect(ui.screen(), Color(0.0, 0.0, 0.0, 0.25 * alpha), 0.0);
    ui.shadow(panel, Color(0.0, 0.0, 0.0, 0.35 * alpha), 30.0);
    ui.rect(panel, t.panel.mul_alpha(alpha), if compact { 0.0 } else { t.radius + 4.0 });
    ui.outline(panel, t.border.mul_alpha(alpha), if compact { 0.0 } else { t.radius + 4.0 }, 1.0);
    ui.block(panel);
    let (nav, content) = if compact {
        let (top, rest) = panel.split_top(56.0);
        (top, rest)
    } else {
        panel.split_left(220.0)
    };
    // Navigation.
    if compact {
        let w = (nav.w - 56.0) / Page::ALL.len() as f32;
        for (i, p) in Page::ALL.iter().enumerate() {
            let r = Rect::new(nav.x + i as f32 * w, nav.y + 6.0, w, 44.0);
            let active = app.view.page == *p;
            let resp = ui.interact(child(id("nav"), i as u64), r);
            if active {
                ui.rect(r.pad(4.0, 2.0), t.surface_active, 10.0);
            }
            ui.icon(r, p.icon(), 21.0, if active { t.text } else { t.text_dim });
            if resp.clicked {
                app.view.page = *p;
            }
        }
        if ui.icon_button(id("nav-close"), Rect::new(nav.right() - 50.0, nav.y + 8.0, 40.0, 40.0), Icon::Close, 20.0, false, "").clicked {
            app.view.menu_open = false;
        }
    } else {
        let mut y = nav.y + 20.0;
        ui.icon(Rect::new(nav.x + 18.0, y, 28.0, 28.0), Icon::Sparkle, 22.0, t.primary_hover);
        ui.text(Rect::new(nav.x + 52.0, y, nav.w - 60.0, 28.0), "OpenChroma", 17.0, Weight::Bold, t.text, Align::Left);
        y += 52.0;
        for (i, p) in Page::ALL.iter().enumerate() {
            let r = Rect::new(nav.x + 10.0, y, nav.w - 20.0, 40.0);
            let active = app.view.page == *p;
            let resp = ui.interact(child(id("nav"), i as u64), r);
            let h = ui.anim(child(id("nav-h"), i as u64), if resp.hovered || active { 1.0 } else { 0.0 }, 16.0);
            if h > 0.0 {
                ui.rect(r, if active { t.surface_active } else { t.surface_hover.mul_alpha(h) }, 8.0);
            }
            if active {
                ui.rect(Rect::new(r.x, r.y + 10.0, 3.0, 20.0), t.primary, 2.0);
            }
            ui.icon(Rect::new(r.x + 12.0, r.y, 22.0, r.h), p.icon(), 19.0, if active { t.text } else { t.text_dim });
            ui.text(Rect::new(r.x + 44.0, r.y, r.w - 50.0, r.h), app.tr(p.label()), 14.0, Weight::Medium, if active { t.text } else { t.text_dim }, Align::Left);
            if resp.clicked {
                app.view.page = *p;
            }
            if resp.hovered {
                ui.ctx.wants_pointer_cursor = true;
            }
            y += 44.0;
        }
        // Server status at the bottom of the navigation.
        let status_r = Rect::new(nav.x + 14.0, nav.bottom() - 44.0, nav.w - 28.0, 32.0);
        let (label, color) = match &app.server.state {
            crate::server::ServerState::Connected(i) => (format!("{} · {}", app.tr("Server"), i.name), t.success),
            crate::server::ServerState::NeedsLogin(_) => (app.tr("Login required").to_string(), t.warning),
            crate::server::ServerState::Connecting => (app.tr("Connecting…").to_string(), t.text_dim),
            crate::server::ServerState::Error(_) => (app.tr("Server offline").to_string(), t.danger),
            crate::server::ServerState::Disabled => (app.tr("Local mode").to_string(), t.text_faint),
        };
        ui.rect(Rect::new(status_r.x, status_r.y + 12.0, 8.0, 8.0), color, 4.0);
        ui.text(Rect::new(status_r.x + 16.0, status_r.y, status_r.w - 16.0, status_r.h), &label, 12.0, Weight::Medium, t.text_dim, Align::Left);
        ui.rect(Rect::new(nav.right() - 1.0, nav.y + 16.0, 1.0, nav.h - 32.0), t.border, 0.0);
    }
    let content = content.pad(if compact { 12.0 } else { 24.0 }, if compact { 8.0 } else { 20.0 });
    match app.view.page {
        Page::Library => library::draw(app, ui, host, content),
        Page::Queue => queue::draw(app, ui, host, content),
        Page::Playlists => playlists::draw(app, ui, host, content),
        Page::Backgrounds => backgrounds::draw(app, ui, host, content),
        Page::Effects => effects::draw(app, ui, host, content),
        Page::Settings => settings::draw(app, ui, host, content),
    }
    // Clicking outside the panel (desktop) closes the menu.
    if !compact && ui.input.pressed[0] {
        if let Some((x, y)) = ui.input.pointer {
            if !panel.contains(x, y) && area.contains(x, y) && app.view.dialog.is_none() && !ui.ctx.has_popup() {
                app.view.menu_open = false;
            }
        }
    }
}

fn toasts(app: &mut App, ui: &mut Ui, screen: Rect, bar_h: f32) {
    let t = ui.theme().clone();
    let mut y = screen.h - bar_h - 12.0;
    for toast in app.view.toasts.clone().iter().rev() {
        let w = (ui.measure(&toast.text, 13.0, Weight::Medium) + 48.0).min(screen.w - 32.0);
        let r = Rect::new(screen.w - w - 16.0, y - 40.0, w, 40.0);
        let remaining = (toast.until - app.time) as f32;
        let a = remaining.clamp(0.0, 0.4) / 0.4;
        ui.shadow(r, Color(0.0, 0.0, 0.0, 0.3 * a), 12.0);
        ui.rect(r, t.panel_strong.mul_alpha(a), 10.0);
        ui.rect(Rect::new(r.x, r.y + 8.0, 3.0, r.h - 16.0), if toast.error { t.danger } else { t.primary }.mul_alpha(a), 2.0);
        ui.icon(Rect::new(r.x + 10.0, r.y, 20.0, r.h), if toast.error { Icon::Warning } else { Icon::Info }, 16.0, t.text.mul_alpha(a));
        ui.text(Rect::new(r.x + 36.0, r.y, r.w - 44.0, r.h), &toast.text, 13.0, Weight::Medium, t.text.mul_alpha(a), Align::Left);
        y -= 48.0;
    }
}
