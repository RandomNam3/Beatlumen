//! Effects page: preset management, effect list and editor, analysis
//! settings with live curves.

use super::common::*;
use crate::app::{AnalysisStatus, App, PresetOrigin};
use crate::host::Host;
use crate::view::{BrowseFor, ConfirmAction, Dialog, EffectsTab, FileBrowser};
use oc_fx::schema::{self, Field, FieldKind, TriggerKind};
use oc_fx::Effect;
use oc_json::Value;
use oc_ui::{child, id, Align, Color, Icon, MenuItem, Rect, RectExt, Ui, Weight};

pub fn draw(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let compact = r.w < 720.0;
    // Header: title, preset selector and actions.
    let (head, rest) = r.split_top(if compact { 132.0 } else { 88.0 });
    ui.text(Rect::new(head.x, head.y, 200.0, 34.0), app.tr("Effects"), t.font_title, Weight::Bold, t.text, Align::Left);
    let names: Vec<String> = app
        .preset_list
        .iter()
        .map(|p| match p.origin {
            PresetOrigin::Builtin => format!("{} ({})", p.name, app.tr("built-in")),
            PresetOrigin::User => p.name.clone(),
            PresetOrigin::Server => format!("{} ({})", p.name, app.tr("server")),
        })
        .collect();
    let mut sel = app.preset_list.iter().position(|p| p.name == app.preset.name && p.origin == app.preset_origin).unwrap_or(0);
    let row2 = if compact { head.y + 42.0 } else { head.y + 46.0 };
    let sel_w = if compact { head.w - 50.0 } else { 280.0 };
    if ui.select(id("preset-select"), Rect::new(head.x, row2, sel_w, 36.0), &mut sel, &names) {
        if let Some(entry) = app.preset_list.get(sel).cloned() {
            app.load_preset_by_name(host, &entry.name);
        }
    }
    if app.preset_dirty {
        ui.rect(Rect::new(head.x + sel_w + 6.0, row2 + 14.0, 8.0, 8.0), t.warning, 4.0);
    }
    let (bx, by) = if compact { (head.x, row2 + 46.0) } else { (head.x + sel_w + 24.0, row2) };
    let mut x = bx;
    if tool_button(ui, &mut x, by, id("preset-save"), app.tr("Save"), Some(Icon::Save), app.preset_dirty) {
        if app.preset_origin == PresetOrigin::Builtin {
            app.view.dialog = Some(Dialog::SavePresetAs(format!("{} {}", app.preset.name, app.tr("(copy)"))));
        } else {
            let (name, srv) = (app.preset.name.clone(), app.preset_origin == PresetOrigin::Server);
            app.save_preset_as(host, &name, srv);
        }
    }
    let more_r = Rect::new(x, by, 40.0, 34.0);
    if ui.icon_button(id("preset-more"), more_r, Icon::More, 20.0, false, app.tr("More")).clicked {
        let items = vec![
            MenuItem::new(app.tr("Save as…")).icon(Icon::Save),
            MenuItem::new(app.tr("New empty preset")).icon(Icon::Plus),
            MenuItem::new(app.tr("Import .hfx…")).icon(Icon::Upload),
            MenuItem::new(app.tr("Export .hfx…")).icon(Icon::Copy).enabled(host.has_filesystem()),
            MenuItem::new(app.tr("Undo")).icon(Icon::Undo).enabled(!app.undo.is_empty()),
            MenuItem::new(app.tr("Redo")).icon(Icon::Redo).enabled(!app.redo.is_empty()),
            MenuItem::new(app.tr("Delete preset")).icon(Icon::Trash).enabled(app.preset_origin != PresetOrigin::Builtin).danger(),
        ];
        ui.open_menu(id("preset-more"), more_r, items, 220.0);
    }
    if let Some(a) = ui.menu_result(id("preset-more")) {
        match a {
            0 => app.view.dialog = Some(Dialog::SavePresetAs(app.preset.name.clone())),
            1 => {
                let p = oc_fx::Preset { name: app.tr("New Preset").to_string(), ..Default::default() };
                app.preset_origin = PresetOrigin::User;
                app.apply_new_preset(p);
            }
            2 => {
                if host.has_filesystem() {
                    app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::ImportPreset, path: home(host), selected: Vec::new() }));
                } else {
                    host.pick_files(".hfx,application/json", false);
                }
            }
            3 => app.view.dialog = Some(Dialog::FileBrowser(FileBrowser { purpose: BrowseFor::ExportPreset, path: home(host), selected: Vec::new() })),
            4 => app.undo_preset(),
            5 => app.redo_preset(),
            6 => {
                app.view.dialog = Some(Dialog::Confirm {
                    title: app.tr("Delete preset").into(),
                    message: format!("{} \"{}\"?", app.tr("Delete the preset"), app.preset.name),
                    action: ConfirmAction::DeletePreset(app.preset.name.clone()),
                })
            }
            _ => {}
        }
    }
    // Tabs.
    let tabs_r = if compact { Rect::new(head.right() - 0.0, head.y, 0.0, 0.0) } else { Rect::new(head.right() - 260.0, head.y, 260.0, 36.0) };
    let mut tab = if app.view.effects_tab == EffectsTab::Effects { 0 } else { 1 };
    let labels = [app.tr("Effects"), app.tr("Analysis")];
    let tabs_r = if compact { Rect::new(head.x + 160.0, head.y, head.w - 160.0, 36.0) } else { tabs_r };
    if ui.tabs(id("fx-tabs"), tabs_r, &mut tab, &labels) {
        app.view.effects_tab = if tab == 0 { EffectsTab::Effects } else { EffectsTab::Analysis };
    }
    let rest = rest.pad(0.0, 4.0);
    match app.view.effects_tab {
        EffectsTab::Effects => effects_tab(app, ui, rest, compact),
        EffectsTab::Analysis => analysis_tab(app, ui, host, rest),
    }
}

fn home(host: &dyn Host) -> String {
    host.fs_roots().first().map(|(_, p)| p.clone()).unwrap_or_else(|| "/".into())
}

fn trigger_label(app: &App, e: &Effect) -> String {
    match e.spec().map(|s| s.trigger) {
        Some(TriggerKind::Novelty) | Some(TriggerKind::Transition) => {
            let tt = e.trigger_type();
            if tt == "None" {
                "–".to_string()
            } else {
                app.tr(tt).to_string()
            }
        }
        _ => "–".to_string(),
    }
}

fn effects_tab(app: &mut App, ui: &mut Ui, r: Rect, compact: bool) {
    let selected = app.view.selected_effect.clone().filter(|s| app.preset.find_effect(s).is_some());
    if compact {
        if let Some(sel) = selected {
            editor(app, ui, r, &sel, true);
        } else {
            effect_list(app, ui, r);
        }
    } else {
        let (list_r, ed_r) = r.split_left((r.w * 0.42).clamp(300.0, 460.0));
        effect_list(app, ui, Rect::new(list_r.x, list_r.y, list_r.w - 16.0, list_r.h));
        match selected {
            Some(sel) => editor(app, ui, ed_r, &sel, false),
            None => empty_state(ui, ed_r, Icon::Sparkle, app.tr("Select an effect"), app.tr("Choose an effect on the left to edit it, or add a new one.")),
        }
    }
}

fn effect_list(app: &mut App, ui: &mut Ui, r: Rect) {
    let t = ui.theme().clone();
    let (bar, rest) = r.split_top(44.0);
    let add_r = Rect::new(bar.x, bar.y, 150.0, 34.0);
    if ui.button(id("fx-add"), add_r, app.tr("Add effect"), Some(Icon::Plus), true).clicked {
        let items: Vec<MenuItem> = schema::EFFECTS.iter().map(|s| MenuItem::new(format!("{} · {}", app.tr(s.effect_type), app.tr(s.ty)))).collect();
        ui.open_menu(id("fx-add"), add_r, items, 260.0);
    }
    if let Some(i) = ui.menu_result(id("fx-add")) {
        if let Some(spec) = schema::EFFECTS.get(i) {
            let before = app.preset.clone();
            let e = Effect::new(spec, &mut app.rng);
            app.view.selected_effect = Some(e.id.clone());
            app.preset.effects.push(e);
            app.preset_edited(before);
        }
    }
    let shadowed = app.preset.shadowed_effects();
    let effects = app.preset.effects.clone();
    let row_h = 52.0;
    let sid = id("fx-list");
    let off = ui.begin_scroll(sid, rest, effects.len() as f32 * row_h);
    let mut select = None;
    let mut toggle = None;
    let mut menu = None;
    for (i, e) in effects.iter().enumerate() {
        let rr = Rect::new(rest.x, rest.y + i as f32 * row_h - off, rest.w - 10.0, row_h - 2.0);
        if rr.bottom() < rest.y || rr.y > rest.bottom() {
            continue;
        }
        let resp = ui.interact(child(sid, i as u64 + 1), rr);
        let selected = app.view.selected_effect.as_deref() == Some(e.id.as_str());
        row_background(ui, rr, resp.hovered, selected, i);
        let mut on = !e.disabled;
        if ui.toggle(child(sid, 100_000 + i as u64), Rect::new(rr.x + 6.0, rr.y, 40.0, rr.h), &mut on) {
            toggle = Some((i, !on));
        }
        let icon = match e.effect_type.as_str() {
            "Camera" => Icon::Eye,
            "Particle" => Icon::Sparkle,
            "Filter" => Icon::Tune,
            _ => Icon::Shuffle,
        };
        let alpha = if e.disabled { 0.45 } else { 1.0 };
        ui.icon(Rect::new(rr.x + 56.0, rr.y, 24.0, rr.h), icon, 16.0, t.text_dim.mul_alpha(alpha));
        ui.text(Rect::new(rr.x + 88.0, rr.y + 7.0, rr.w - 170.0, 20.0), &e.name, 14.0, Weight::Medium, t.text.mul_alpha(alpha), Align::Left);
        let sub = format!("{} · {}", app.tr(&e.effect_type), app.tr(&e.ty));
        ui.text(Rect::new(rr.x + 88.0, rr.y + 27.0, rr.w - 170.0, 16.0), &sub, 11.0, Weight::Regular, t.text_faint.mul_alpha(alpha), Align::Left);
        let trig = trigger_label(app, e);
        ui.text(Rect::new(rr.right() - 80.0, rr.y, 50.0, rr.h), &trig, 12.0, Weight::Medium, t.text_dim.mul_alpha(alpha), Align::Right);
        if shadowed.contains(&e.id) {
            ui.icon(Rect::new(rr.right() - 26.0, rr.y, 20.0, rr.h), Icon::Warning, 14.0, t.warning);
            ui.tooltip(child(sid, i as u64 + 1), rr, app.tr("A later effect of the same type overrides this one."));
        }
        if resp.clicked && toggle.is_none() && !ui.is_drag_scrolling() {
            select = Some(e.id.clone());
        }
        if secondary(ui, &resp, rr) {
            menu = Some(i);
        }
    }
    ui.end_scroll(sid, rest);
    if let Some(s) = select {
        app.view.selected_effect = Some(s);
    }
    if let Some((i, disabled)) = toggle {
        let before = app.preset.clone();
        app.preset.effects[i].disabled = disabled;
        app.preset_edited(before);
    }
    if let Some(i) = menu {
        app.view.context_items = vec![i.to_string()];
        let items = vec![
            MenuItem::new(app.tr("Duplicate")).icon(Icon::Copy),
            MenuItem::new(app.tr("Move up")).icon(Icon::ChevronUp).enabled(i > 0),
            MenuItem::new(app.tr("Move down")).icon(Icon::ChevronDown).enabled(i + 1 < effects.len()),
            MenuItem::new(app.tr("Delete")).icon(Icon::Trash).danger(),
        ];
        ui.open_context_menu(id("fx-menu"), items);
    }
    if let Some(a) = ui.menu_result(id("fx-menu")) {
        let i: usize = app.view.context_items.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        effect_action(app, i, a);
    }
}

fn effect_action(app: &mut App, i: usize, action: usize) {
    if i >= app.preset.effects.len() {
        return;
    }
    let before = app.preset.clone();
    match action {
        0 => {
            let mut e = app.preset.effects[i].clone();
            e.id = app.rng.id();
            e.name = format!("{} {}", e.name, app.tr("(copy)"));
            app.view.selected_effect = Some(e.id.clone());
            app.preset.effects.insert(i + 1, e);
        }
        1 if i > 0 => app.preset.effects.swap(i, i - 1),
        2 if i + 1 < app.preset.effects.len() => app.preset.effects.swap(i, i + 1),
        3 => {
            let e = app.preset.effects.remove(i);
            if app.view.selected_effect.as_deref() == Some(e.id.as_str()) {
                app.view.selected_effect = None;
            }
        }
        _ => return,
    }
    app.preset_edited(before);
}

/// Number control: slider for bounded fields, stepper otherwise.
fn number_control(ui: &mut Ui, wid: u64, r: Rect, value: f64, f: &Field) -> Option<f64> {
    let t = ui.theme().clone();
    let integer = matches!(f.kind, FieldKind::Integer);
    let bounded = f.min.is_finite() && f.max.is_finite() && (f.max - f.min) <= 1000.0;
    let label = if integer { format!("{}", value.round() as i64) } else { trim_float(value) };
    let label = format!("{label}{}", f.unit);
    if bounded {
        let (sr, lr) = r.split_right(72.0);
        let mut v = value as f32;
        let step = if f.step > 0.0 { f.step as f32 } else { 0.01 };
        let changed = ui.slider(wid, Rect::new(sr.x, sr.y, sr.w - 10.0, sr.h), &mut v, f.min as f32, f.max as f32, step);
        ui.text(lr, &label, 13.0, Weight::Medium, t.text, Align::Right);
        changed.then(|| v as f64)
    } else {
        let step = if f.step > 0.0 { f.step } else { 0.1 };
        let (minus, rest) = r.split_left(34.0);
        let (mid, plus) = rest.split_right(34.0);
        let mut out = None;
        if ui.button(child(wid, 1), minus, "", Some(Icon::Minus), false).clicked {
            out = Some(value - step);
        }
        if ui.button(child(wid, 2), plus, "", Some(Icon::Plus), false).clicked {
            out = Some(value + step);
        }
        // Drag on the value to change it.
        let resp = ui.interact(child(wid, 3), mid);
        ui.rect(mid.pad(4.0, 0.0), Color(0.0, 0.0, 0.0, 0.2), 6.0);
        ui.text(mid, &label, 13.0, Weight::Medium, t.text, Align::Center);
        if resp.held && !ui.input.touch && ui.input.drag_delta.0 != 0.0 {
            out = Some(value + ui.input.drag_delta.0 as f64 * step * 0.25);
        }
        if resp.hovered && ui.input.scroll.1 != 0.0 {
            out = Some(value + ui.input.scroll.1.signum() as f64 * step);
        }
        if resp.hovered {
            ui.ctx.wants_pointer_cursor = true;
        }
        out.map(|v| {
            let v = v.clamp(f.min, f.max);
            if integer {
                v.round()
            } else {
                (v / step).round() * step
            }
        })
    }
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// Draws a field of `obj` and returns the new value when edited.
fn field_control(app: &App, ui: &mut Ui, wid: u64, r: Rect, obj: &Value, f: &Field) -> Option<Value> {
    match f.kind {
        FieldKind::Number | FieldKind::Integer => {
            let v = obj.get(f.key).as_f64().unwrap_or(f.default);
            number_control(ui, wid, r, v, f).map(Value::from)
        }
        FieldKind::Easing => {
            let cur = obj.get(f.key).as_str().unwrap_or(f.default_str);
            let names: Vec<String> = oc_fx::easing::ALL.iter().map(|(_, n)| n.to_string()).collect();
            let mut i = oc_fx::easing::Easing::from_name(cur).and_then(|e| oc_fx::easing::ALL.iter().position(|(x, _)| *x == e)).unwrap_or(0);
            ui.select(wid, r, &mut i, &names).then(|| Value::from(names[i].as_str()))
        }
        FieldKind::Enum(values) => {
            let cur = obj.get(f.key).as_str().unwrap_or(f.default_str);
            let labels: Vec<String> = values.iter().map(|(_, l)| app.tr(l).to_string()).collect();
            let mut i = values.iter().position(|(v, _)| *v == cur).unwrap_or(0);
            ui.select(wid, r, &mut i, &labels).then(|| Value::from(values[i].0))
        }
        FieldKind::IntEnum(values) => {
            let cur = obj.get(f.key).as_i64().unwrap_or(f.default as i64);
            let labels: Vec<String> = values.iter().map(|(_, l)| app.tr(l).to_string()).collect();
            let mut i = values.iter().position(|(v, _)| *v == cur).unwrap_or(0);
            ui.select(wid, r, &mut i, &labels).then(|| Value::from(values[i].0))
        }
        FieldKind::Color => {
            let cur = obj.get(f.key).as_u64().unwrap_or(f.default as u64) as u32;
            let swatch = Rect::new(r.x, r.y, 34.0, r.h);
            ui.rect(swatch, oc_ui::Color::hex(cur), 6.0);
            ui.outline(swatch, ui.theme().border, 6.0, 1.0);
            // Preset colours.
            let palette = [0xffffff, 0xfff1d6, 0xffd1e8, 0xd6e8ff, 0xc9ffe5, 0xffb38a, 0xa98bff, 0x66d9ff];
            let mut out = None;
            for (i, c) in palette.iter().enumerate() {
                let cr = Rect::new(r.x + 44.0 + i as f32 * 30.0, r.y + 5.0, 24.0, 24.0);
                if cr.right() > r.right() {
                    break;
                }
                let resp = ui.interact(child(wid, i as u64), cr);
                ui.rect(cr, oc_ui::Color::hex(*c), 12.0);
                if *c == cur {
                    ui.outline(cr.shrink(-3.0), ui.theme().text, 14.0, 1.5);
                }
                if resp.clicked {
                    out = Some(Value::from(*c));
                }
            }
            out
        }
    }
}

fn editor(app: &mut App, ui: &mut Ui, r: Rect, effect_id: &str, compact: bool) {
    let t = ui.theme().clone();
    let Some(effect) = app.preset.find_effect(effect_id).cloned() else { return };
    let Some(spec) = effect.spec() else { return };
    let (head, body) = r.split_top(52.0);
    let mut x = head.x;
    if compact {
        if ui.icon_button(id("fx-back"), Rect::new(head.x - 6.0, head.y, 36.0, 36.0), Icon::ChevronLeft, 22.0, false, app.tr("Back")).clicked {
            app.view.selected_effect = None;
        }
        x += 36.0;
    }
    // Name (editable).
    let mut name = effect.name.clone();
    let (changed, _) = ui.text_input(id("fx-name"), Rect::new(x, head.y, (head.w - (x - head.x) - 50.0).min(360.0), 36.0), &mut name, "", false);
    if changed {
        let before = app.preset.clone();
        if let Some(e) = app.preset.find_effect_mut(effect_id) {
            e.name = name;
        }
        app.preset_edited_coalesced(before, "name");
    }
    if ui.icon_button(id("fx-delete"), Rect::new(head.right() - 40.0, head.y, 36.0, 36.0), Icon::Trash, 18.0, false, app.tr("Delete effect")).clicked {
        if let Some(i) = app.preset.effects.iter().position(|e| e.id == effect_id) {
            effect_action(app, i, 3);
        }
        return;
    }
    // Field list (scrollable).
    struct Row {
        section: Option<String>,
        block: &'static str,
        field: Option<Field>,
    }
    let mut rows: Vec<Row> = Vec::new();
    rows.push(Row { section: Some(format!("{} · {}", app.tr(spec.effect_type), app.tr(spec.ty))), block: "", field: None });
    for f in spec.config {
        rows.push(Row { section: None, block: "config", field: Some(*f) });
    }
    match spec.trigger {
        TriggerKind::Novelty => {
            rows.push(Row { section: Some(app.tr("Trigger").to_string()), block: "", field: None });
            for f in schema::NOVELTY_TRIGGER {
                rows.push(Row { section: None, block: "trigger", field: Some(*f) });
            }
            if !spec.targets.is_empty() {
                rows.push(Row { section: Some(app.tr("Trigger animation (target values)").to_string()), block: "", field: None });
                for f in spec.targets {
                    rows.push(Row { section: None, block: "trigger", field: Some(*f) });
                }
            }
        }
        TriggerKind::Transition => {
            rows.push(Row { section: Some(app.tr("Trigger").to_string()), block: "", field: None });
            for f in schema::TRANSITION_TRIGGER {
                rows.push(Row { section: None, block: "trigger", field: Some(*f) });
            }
        }
        TriggerKind::None => {}
    }
    if spec.camera {
        rows.push(Row { section: Some(app.tr("Camera").to_string()), block: "", field: None });
        rows.push(Row { section: None, block: "camera", field: Some(schema::CAMERA_SPEED) });
        for (block, title, fields) in [
            ("camera.exit", "Camera exit (outgoing image)", schema::CAMERA_EXIT),
            ("camera.enter", "Camera enter (incoming image)", schema::CAMERA_ENTER),
            ("camera.finish", "Camera finish (incoming image)", schema::CAMERA_FINISH),
        ] {
            rows.push(Row { section: Some(app.tr(title).to_string()), block: "", field: None });
            for f in fields {
                rows.push(Row { section: None, block, field: Some(*f) });
            }
        }
    }
    let row_h = 46.0;
    let sid = child(id("fx-editor"), oc_ui::id(effect_id));
    let off = ui.begin_scroll(sid, body, rows.len() as f32 * row_h + 20.0);
    let mut edit: Option<(&'static str, &'static str, Value)> = None;
    for (i, row) in rows.iter().enumerate() {
        let rr = Rect::new(body.x, body.y + i as f32 * row_h - off, body.w - 16.0, row_h);
        if rr.bottom() < body.y || rr.y > body.bottom() {
            continue;
        }
        if let Some(s) = &row.section {
            section_title(ui, Rect::new(rr.x, rr.y + 16.0, rr.w, 24.0), &s.to_uppercase());
            ui.rect(Rect::new(rr.x, rr.bottom() - 4.0, rr.w, 1.0), t.border, 0.0);
            continue;
        }
        let Some(f) = row.field else { continue };
        let obj = match row.block {
            "config" => effect.config.clone(),
            "trigger" => effect.trigger.clone(),
            "camera" => effect.camera.clone(),
            "camera.exit" => effect.camera.get("exit").clone(),
            "camera.enter" => effect.camera.get("enter").clone(),
            "camera.finish" => effect.camera.get("finish").clone(),
            _ => Value::Null,
        };
        let control = field_row(ui, rr, app.tr(f.label), "");
        let wid = child(sid, oc_ui::id(row.block) ^ oc_ui::id(f.key));
        if let Some(v) = field_control(app, ui, wid, control, &obj, &f) {
            edit = Some((row.block, f.key, v));
        }
    }
    ui.end_scroll(sid, body);
    if let Some((block, key, v)) = edit {
        let before = app.preset.clone();
        if let Some(e) = app.preset.find_effect_mut(effect_id) {
            match block {
                "config" => e.config.set(key, v),
                "trigger" => e.trigger.set(key, v),
                "camera" => e.camera.set(key, v),
                b => {
                    let sub = b.trim_start_matches("camera.");
                    let mut o = e.camera.get(sub).clone();
                    o.set(key, v);
                    e.camera.set(sub, o);
                }
            }
        }
        app.preset_edited_coalesced(before, &format!("{effect_id}/{block}/{key}"));
    }
}

fn analysis_tab(app: &mut App, ui: &mut Ui, host: &mut dyn Host, r: Rect) {
    let t = ui.theme().clone();
    let compact = r.w < 820.0;
    let (form_r, graphs_r) = if compact { r.split_top(r.h * 0.55) } else { r.split_left(r.w * 0.46) };
    // Settings form.
    let mut rows: Vec<(Option<String>, &'static str, Option<Field>)> = Vec::new();
    rows.push((Some(app.tr("Structure (transitions)").to_string()), "", None));
    for f in schema::ANALYSIS {
        rows.push((None, "analysis", Some(*f)));
    }
    for band in ["Low", "Mid", "High"] {
        rows.push((Some(format!("{} – {}", app.tr("Novelty"), app.tr(band))), "", None));
        for f in schema::NOVELTY {
            rows.push((None, band, Some(*f)));
        }
    }
    let row_h = 46.0;
    let sid = id("analysis-form");
    let form_inner = Rect::new(form_r.x, form_r.y + 44.0, form_r.w - 16.0, form_r.h - 44.0);
    let mut x = form_r.x;
    if tool_button(ui, &mut x, form_r.y, id("analysis-defaults"), app.tr("Restore defaults"), Some(Icon::Refresh), false) {
        let before = app.preset.clone();
        app.preset.analysis = Value::Null;
        app.preset.novelty = Value::Null;
        app.preset.normalize();
        app.preset_edited(before);
    }
    let status = match app.analysis_status {
        AnalysisStatus::None => app.tr("No track"),
        AnalysisStatus::Working => app.tr("Analysing…"),
        AnalysisStatus::Ready => app.tr("Analysis ready"),
        AnalysisStatus::Failed => app.tr("Analysis unavailable (live fallback)"),
    };
    ui.text(Rect::new(x + 8.0, form_r.y, form_r.right() - x - 8.0, 34.0), status, 12.0, Weight::Medium, t.text_dim, Align::Left);
    let off = ui.begin_scroll(sid, form_inner, rows.len() as f32 * row_h + 10.0);
    let mut edit = None;
    for (i, (section, block, field)) in rows.iter().enumerate() {
        let rr = Rect::new(form_inner.x, form_inner.y + i as f32 * row_h - off, form_inner.w - 8.0, row_h);
        if rr.bottom() < form_inner.y || rr.y > form_inner.bottom() {
            continue;
        }
        if let Some(s) = section {
            section_title(ui, Rect::new(rr.x, rr.y + 16.0, rr.w, 24.0), &s.to_uppercase());
            continue;
        }
        let Some(f) = field else { continue };
        let obj = if *block == "analysis" { app.preset.analysis.clone() } else { app.preset.novelty.get(block).clone() };
        let control = field_row(ui, rr, app.tr(f.label), "");
        let wid = child(sid, oc_ui::id(block) ^ oc_ui::id(f.key));
        if let Some(v) = field_control(app, ui, wid, control, &obj, f) {
            edit = Some((*block, f.key, v));
        }
    }
    ui.end_scroll(sid, form_inner);
    if let Some((block, key, v)) = edit {
        let before = app.preset.clone();
        if block == "analysis" {
            app.preset.analysis.set(key, v);
        } else {
            let mut o = app.preset.novelty.get(block).clone();
            o.set(key, v);
            app.preset.novelty.set(block, o);
        }
        app.preset_edited_coalesced(before, &format!("analysis/{block}/{key}"));
    }
    // Graphs.
    let g = if compact { graphs_r.pad(0.0, 8.0) } else { graphs_r.pad(16.0, 0.0) };
    let h = (g.h - 3.0 * 30.0) / 3.0;
    let pos = app.player.position(host);
    let colors = [t.primary, Color(0.4, 0.7, 1.0, 1.0), Color(0.5, 1.0, 0.7, 1.0)];
    let analysis = app.analysis.clone();
    let mut y = g.y;
    for (gi, (title, window)) in [(app.tr("Energy"), 5.0), (app.tr("Novelty (Lows · Mids · Highs)"), 5.0), (app.tr("Transitions (Major · Minor)"), 30.0)].iter().enumerate() {
        ui.text(Rect::new(g.x, y, g.w, 26.0), title, 12.0, Weight::SemiBold, t.text_faint, Align::Left);
        let gr = Rect::new(g.x, y + 26.0, g.w, h.max(40.0));
        ui.rect(gr, Color(0.0, 0.0, 0.0, 0.25), 8.0);
        if let Some(a) = &analysis {
            let rate = a.frame_rate();
            let start = (pos - window * 0.2).max(0.0);
            match gi {
                0 => plot(ui, gr, &a.energy, rate, start, *window, colors[0], 4.0),
                1 => {
                    for (ci, curve) in a.novelty.iter().enumerate().take(3) {
                        plot(ui, gr, curve, rate, start, *window, colors[ci].with_alpha(0.85), 1.0);
                    }
                }
                _ => {
                    let trate = rate / a.transition_down_sampling as f64;
                    let n = a.transitions.len();
                    let i0 = (start * trate) as usize;
                    let i1 = (((start + window) * trate).ceil() as usize).min(n);
                    for i in i0..i1 {
                        let v = a.transitions[i];
                        if v <= 0.0 {
                            continue;
                        }
                        let x = gr.x + ((i as f64 / trate - start) / window) as f32 * gr.w;
                        let hh = gr.h * v * 0.9;
                        ui.rect(Rect::new(x, gr.bottom() - hh, 3.0, hh), if v >= 1.0 { colors[0] } else { colors[1] }, 1.0);
                    }
                }
            }
            // Playhead.
            let px = gr.x + ((pos - start) / window) as f32 * gr.w;
            ui.rect(Rect::new(px, gr.y, 1.5, gr.h), Color(1.0, 1.0, 1.0, 0.7), 0.0);
        } else {
            ui.text(gr, app.tr("No analysis available"), 12.0, Weight::Regular, t.text_faint, Align::Center);
        }
        y += 26.0 + h.max(40.0) + 4.0;
    }
}

/// Plots a curve sampled at `rate` values/second over [start, start+window].
#[allow(clippy::too_many_arguments)]
fn plot(ui: &mut Ui, r: Rect, data: &[f32], rate: f64, start: f64, window: f64, color: Color, gain: f32) {
    let cols = (r.w / 2.0) as usize;
    for c in 0..cols {
        let t0 = start + window * c as f64 / cols as f64;
        let t1 = start + window * (c + 1) as f64 / cols as f64;
        let i0 = (t0 * rate) as usize;
        let i1 = ((t1 * rate) as usize).max(i0 + 1).min(data.len());
        if i0 >= data.len() {
            break;
        }
        let v = data[i0..i1].iter().cloned().fold(0.0f32, f32::max);
        let hh = ((v * gain).clamp(0.0, 1.0)) * (r.h - 6.0);
        if hh > 0.5 {
            ui.rect(Rect::new(r.x + c as f32 * 2.0, r.bottom() - 3.0 - hh, 2.0, hh), color, 0.0);
        }
    }
}
