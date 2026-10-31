//! The JSON command API used by the user interface.
//!
//! Every command is an object `{"cmd": "<name>", ...arguments}`; the result
//! is a JSON value (or `{"error": "..."}`). Commands that change state also
//! bump the matching revision counter in [`Core::state`], so the UI knows
//! which lists to refetch.

use crate::core::{Core, PresetOrigin, Req, DEFAULT_BG};
use crate::host::Host;
use crate::library::{self, LOCAL_PREFIX};
use crate::settings::{Repeat, Settings};
use bl_fx::schema::{self, Field, FieldKind, TriggerKind};
use bl_fx::Effect;
use bl_json::Value;
use bl_net::api::{Playlist, PlaylistKind, Track};

fn err(msg: impl Into<String>) -> Value {
    Value::object().with("error", msg.into())
}

fn ok() -> Value {
    Value::object().with("ok", true)
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A local folder path with "/" separators and no trailing one, for
/// comparing paths that mix "/" and "\" (Windows).
fn folder_key(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_string()
}

/// Whether the local file `path` lies inside `folder`.
fn in_folder(path: &str, folder: &str) -> bool {
    let folder = folder_key(folder);
    let path = path.replace('\\', "/");
    path.strip_prefix(folder.as_str())
        .is_some_and(|r| r.starts_with('/'))
}

/// A form field for the effect / analysis editors.
fn field_json(f: &Field, obj: &Value, tr: &dyn Fn(&str) -> String) -> Value {
    let mut v = Value::object()
        .with("key", f.key)
        .with("label", tr(f.label))
        .with("unit", f.unit);
    match f.kind {
        FieldKind::Number | FieldKind::Integer => {
            let cur = obj.get(f.key).as_f64().unwrap_or(f.default);
            v = v
                .with(
                    "kind",
                    if matches!(f.kind, FieldKind::Integer) {
                        "int"
                    } else {
                        "number"
                    },
                )
                .with("value", cur)
                .with("default", f.default)
                .with(
                    "min",
                    if f.min.is_finite() {
                        Value::from(f.min)
                    } else {
                        Value::Null
                    },
                )
                .with(
                    "max",
                    if f.max.is_finite() {
                        Value::from(f.max)
                    } else {
                        Value::Null
                    },
                )
                .with("step", if f.step > 0.0 { f.step } else { 0.01 });
        }
        FieldKind::Easing => {
            let cur = obj.get(f.key).as_str().unwrap_or(f.default_str).to_string();
            let opts: Vec<Value> = bl_fx::easing::ALL
                .iter()
                .map(|(_, n)| Value::object().with("value", *n).with("label", *n))
                .collect();
            v = v
                .with("kind", "enum")
                .with("value", cur)
                .with("options", Value::Array(opts));
        }
        FieldKind::Enum(values) => {
            let cur = obj.get(f.key).as_str().unwrap_or(f.default_str).to_string();
            let opts: Vec<Value> = values
                .iter()
                .map(|(val, l)| Value::object().with("value", *val).with("label", tr(l)))
                .collect();
            v = v
                .with("kind", "enum")
                .with("value", cur)
                .with("options", Value::Array(opts));
        }
        FieldKind::IntEnum(values) => {
            let cur = obj.get(f.key).as_i64().unwrap_or(f.default as i64);
            let opts: Vec<Value> = values
                .iter()
                .map(|(val, l)| Value::object().with("value", *val).with("label", tr(l)))
                .collect();
            v = v
                .with("kind", "intenum")
                .with("value", cur)
                .with("options", Value::Array(opts));
        }
        FieldKind::Color => {
            let cur = obj.get(f.key).as_u64().unwrap_or(f.default as u64);
            v = v.with("kind", "color").with("value", cur);
        }
    }
    v
}

fn section(
    title: String,
    block: &str,
    fields: &[Field],
    obj: &Value,
    tr: &dyn Fn(&str) -> String,
) -> Value {
    Value::object()
        .with("title", title)
        .with("block", block)
        .with(
            "fields",
            Value::Array(fields.iter().map(|f| field_json(f, obj, tr)).collect()),
        )
}

impl Core {
    fn trf(&self) -> impl Fn(&str) -> String + '_ {
        move |s: &str| self.tr(s).to_string()
    }

    /// Executes a UI command. See the module documentation.
    pub fn command(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let name = c.get("cmd").as_str().unwrap_or("").to_string();
        let s = |k: &str| c.get(k).as_str().unwrap_or("").to_string();
        let n = |k: &str| c.get(k).as_f64().unwrap_or(0.0);
        let b = |k: &str| c.get(k).as_bool().unwrap_or(false);
        let idx = |k: &str| c.get(k).as_u64().unwrap_or(0) as usize;
        // Other apps' playback is shown: the transport controls are theirs.
        if let Some(r) = self.capture_transport(host, &name, c) {
            return r;
        }
        match name.as_str() {
            // ---- state ----------------------------------------------------
            "state" => self.state(host),
            "events" => self.take_events(),

            // ---- playback -------------------------------------------------
            "play" => {
                let ids = strs(c.get("ids"));
                if ids.is_empty() && self.player.state() == crate::player::PlayState::Stopped {
                    // Nothing loaded (e.g. after a restart): the queue's track.
                    self.toggle_play(host);
                } else if ids.is_empty() {
                    self.player.play(host);
                } else {
                    self.play_tracks(host, ids, idx("start"));
                }
                ok()
            }
            "pause" => {
                if self.playing_airplay() {
                    // A speaker cannot pause its sender: stop receiving.
                    self.release_airplay(host);
                } else {
                    self.player.pause(host);
                }
                ok()
            }
            "toggle" => {
                self.toggle_play(host);
                ok()
            }
            "stop" => {
                self.release_airplay(host);
                self.player.stop(host);
                ok()
            }
            "next" => {
                self.next_track(host, true);
                ok()
            }
            "prev" => {
                self.prev_track(host);
                ok()
            }
            "seek" => {
                self.player.seek(host, n("position").max(0.0));
                ok()
            }
            "seekBy" => {
                let p = self.player.position(host);
                self.player.seek(host, (p + n("delta")).max(0.0));
                ok()
            }
            "volume" => {
                self.set_volume(n("value") as f32);
                ok()
            }
            "mute" => {
                let m = if c.get("value").is_null() {
                    !self.settings.muted
                } else {
                    b("value")
                };
                self.set_muted(m);
                ok()
            }
            "shuffle" => {
                let on = if c.get("value").is_null() {
                    !self.queue.shuffle
                } else {
                    b("value")
                };
                self.queue.set_shuffle(on, &mut self.rng);
                self.revs.queue += 1;
                self.settings_changed();
                ok()
            }
            "repeat" => {
                self.settings.repeat = if c.get("value").is_null() {
                    self.settings.repeat.next()
                } else {
                    Repeat::parse(&s("value"))
                };
                self.settings_changed();
                ok()
            }
            "enqueue" => {
                self.enqueue(host, strs(c.get("ids")), b("next"));
                ok()
            }
            "open" => {
                self.open_paths(
                    host,
                    strs(c.get("paths")),
                    !c.get("play").is_null() && b("play") || c.get("play").is_null(),
                );
                ok()
            }

            // ---- queue ----------------------------------------------------
            "queue.list" => {
                let cur = self.queue.position();
                let items: Vec<Value> = self
                    .queue
                    .ordered()
                    .enumerate()
                    .map(|(pos, (_, id))| {
                        let t = self.track_for(id);
                        self.track_json(&t)
                            .with("pos", pos)
                            .with("current", Some(pos) == cur)
                    })
                    .collect();
                Value::Array(items)
            }
            "queue.play" => {
                self.queue.set_position(idx("pos"));
                self.play_current(host, true);
                ok()
            }
            "queue.remove" => {
                let pos = idx("pos");
                let was_current = self.queue.position() == Some(pos);
                self.queue.remove_at(pos);
                if was_current && self.queue.current().is_some() {
                    self.play_current(host, self.is_playing());
                }
                self.revs.queue += 1;
                self.settings_changed();
                ok()
            }
            "queue.move" => {
                self.queue.move_entry(idx("from"), idx("to"));
                self.revs.queue += 1;
                self.settings_changed();
                ok()
            }
            "queue.clear" => {
                self.queue.clear();
                self.player.stop(host);
                self.current = None;
                self.revs.queue += 1;
                self.settings_changed();
                ok()
            }

            // ---- library --------------------------------------------------
            "library.tracks" => {
                let ids = strs(c.get("ids"));
                let tracks: Vec<Value> = if ids.is_empty() {
                    self.library
                        .tracks
                        .iter()
                        .map(|t| self.track_json(t))
                        .collect()
                } else {
                    ids.iter()
                        .map(|id| self.track_json(&self.track_for(id)))
                        .collect()
                };
                Value::Array(tracks)
            }
            "library.search" => {
                let q = s("query");
                let hits = self.library.search(&q);
                Value::Array(
                    hits.into_iter()
                        .take(500)
                        .map(|i| self.track_json(&self.library.tracks[i]))
                        .collect(),
                )
            }
            "library.albums" => Value::Array(
                self.library
                    .albums
                    .iter()
                    .map(|a| {
                        let cover = a
                            .cover_track
                            .as_ref()
                            .or_else(|| self.library.album_tracks(&a.id).first().map(|t| &t.id))
                            .and_then(|id| self.library.track(id))
                            .map(|t| self.cover_url_cached(t))
                            .unwrap_or_default();
                        Value::object()
                            .with("id", a.id.as_str())
                            .with("title", a.name.as_str())
                            .with("artist", a.artist.as_str())
                            .with("year", a.year)
                            .with("tracks", a.track_count)
                            .with("cover", cover)
                            .with(
                                "coverTrack",
                                a.cover_track.clone().or_else(|| {
                                    self.library
                                        .album_tracks(&a.id)
                                        .first()
                                        .map(|t| t.id.clone())
                                }),
                            )
                    })
                    .collect(),
            ),
            "library.album" => {
                let mut tracks: Vec<&Track> = self.library.album_tracks(&s("id"));
                tracks.sort_by_key(|t| {
                    (
                        t.disc.unwrap_or(0),
                        t.track.unwrap_or(0),
                        t.title.to_lowercase(),
                    )
                });
                Value::Array(tracks.into_iter().map(|t| self.track_json(t)).collect())
            }
            "library.artists" => Value::Array(
                self.library
                    .artists
                    .iter()
                    .map(|a| {
                        Value::object()
                            .with("name", a.name.as_str())
                            .with("tracks", a.tracks)
                            .with("albums", a.albums)
                    })
                    .collect(),
            ),
            "library.artist" => Value::Array(
                self.library
                    .artist_tracks(&s("name"))
                    .into_iter()
                    .map(|t| self.track_json(t))
                    .collect(),
            ),
            "library.folder" => self.folder_view(&s("path")),
            "lyrics.get" => self.lyrics_get(host),
            "lyrics.import" => self.lyrics_import(host, &s("text")),
            "lyrics.edit" => self.lyrics_edit(host, c),
            "lyrics.importFile" => match host.fs_read(&s("path")) {
                Ok(d) => {
                    self.lyrics_import_bytes(host, &d);
                    ok()
                }
                Err(e) => err(e),
            },
            "lyrics.expectImport" => {
                self.lyrics.expect_import = true;
                ok()
            }
            "lyrics.remove" => self.lyrics_remove(host),
            "lyrics.sync" => self.lyrics_sync(host),
            n if n.starts_with("devices.") => self.devices_command(host, n, c),
            n if n.starts_with("share.") => self.share_command(host, n, c),
            n if n.starts_with("speech.") => self.speech_command(host, n, c),
            n if n.starts_with("translate.") => self.translate_command(host, n, c),
            n if n.starts_with("sync.") => self.sync_command(host, n),
            "dlna.answer" => {
                self.dlna_answer(host, b("allow"), b("always"));
                ok()
            }
            "dlna.forget" => {
                self.settings.dlna_allowed.clear();
                self.settings_changed();
                ok()
            }
            n if n.starts_with("sources.") || n.starts_with("radio.") => {
                self.sources_command(host, n, c)
            }
            "offline.save" => self.offline_save(host, strs(c.get("ids"))),
            "offline.overview" => self.offline_overview(),
            "offline.remove" => {
                let ids = if c.bool_or("all", false) {
                    self.offline.entries.keys().cloned().collect()
                } else {
                    strs(c.get("ids"))
                };
                self.offline_remove(ids);
                ok()
            }
            "library.rescan" => {
                if !self.settings.library_folders.is_empty() {
                    self.rescan_local();
                }
                if self.server.is_connected() {
                    self.server_rescan(host);
                }
                ok()
            }
            "library.addFolder" => {
                let p = s("path");
                if !p.is_empty() && !self.settings.library_folders.contains(&p) {
                    self.settings.library_folders.push(p);
                    self.settings_changed();
                    self.rescan_local();
                }
                ok()
            }
            "library.removeFolder" => {
                let p = s("path");
                self.settings.library_folders.retain(|f| f != &p);
                self.settings_changed();
                self.rescan_local();
                ok()
            }
            "covers" => {
                self.request_covers(host, strs(c.get("ids")));
                ok()
            }

            // ---- file system (folder picker) ------------------------------
            "fs.roots" => Value::Array(
                host.fs_roots()
                    .into_iter()
                    .map(|(label, path)| Value::object().with("label", label).with("path", path))
                    .collect(),
            ),
            "fs.list" => match host.fs_list(&s("path")) {
                Ok(entries) => {
                    let images_too = b("images");
                    // Files with these extensions only ("*": all files).
                    let exts: Option<Vec<String>> = c.get("exts").as_array().map(|_| {
                        strs(c.get("exts"))
                            .iter()
                            .map(|e| e.to_lowercase())
                            .collect()
                    });
                    Value::Array(
                        entries
                            .into_iter()
                            .filter(|e| {
                                let ext = crate::jobs::extension(&e.name).unwrap_or_default();
                                if e.is_dir {
                                    return true;
                                }
                                if let Some(exts) = &exts {
                                    return exts.iter().any(|x| x == "*" || *x == ext);
                                }
                                bl_media::is_supported_extension(&ext)
                                    || (images_too && library::is_image_extension(&ext))
                                    || ext == "hfx"
                            })
                            .map(|e| {
                                Value::object()
                                    .with("name", e.name)
                                    .with("path", e.path)
                                    .with("dir", e.is_dir)
                                    .with("size", e.size)
                            })
                            .collect(),
                    )
                }
                Err(e) => err(e),
            },
            "fs.parent" => {
                let p = s("path");
                let parent = std::path::Path::new(&p)
                    .parent()
                    .map(|x| x.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Value::from(parent)
            }

            // ---- playlists ------------------------------------------------
            "playlists.list" => {
                let all: Vec<Value> = self
                    .local_playlists
                    .iter()
                    .map(|p| (p, false))
                    .chain(self.server_playlists.iter().map(|p| (p, true)))
                    .map(|(p, srv)| {
                        let cover = p
                            .items
                            .iter()
                            .filter(|_| p.kind == PlaylistKind::Track)
                            .find_map(|id| self.library.track(id))
                            .map(|t| self.cover_url_cached(t))
                            .unwrap_or_default();
                        Value::object()
                            .with("id", p.id.as_str())
                            .with("name", p.name.as_str())
                            .with("kind", p.kind.name())
                            .with("count", p.items.len())
                            .with("server", srv)
                            .with("cover", cover)
                    })
                    .collect();
                Value::Array(all)
            }
            "playlist.items" => match self.find_playlist(&s("id")) {
                Some(p) if p.kind == PlaylistKind::Track => Value::Array(
                    p.items
                        .iter()
                        .map(|id| self.track_json(&self.track_for(id)))
                        .collect(),
                ),
                Some(p) => Value::Array(
                    p.items
                        .iter()
                        .map(|src| self.background_json(src))
                        .collect(),
                ),
                None => err("unknown playlist"),
            },
            "playlist.create" => {
                let kind = if s("kind").eq_ignore_ascii_case("background") {
                    PlaylistKind::Background
                } else {
                    PlaylistKind::Track
                };
                let items = if b("fromQueue") {
                    self.queue.ordered().map(|(_, id)| id.clone()).collect()
                } else {
                    strs(c.get("items"))
                };
                self.create_playlist(host, &s("name"), kind, items, b("server"));
                ok()
            }
            "playlist.add" => {
                if let Some(mut p) = self.find_playlist(&s("id")).cloned() {
                    p.items.extend(strs(c.get("items")));
                    self.update_playlist(host, &p);
                    self.toast(
                        format!("{} \"{}\"", self.tr("Added to playlist"), p.name),
                        false,
                    );
                }
                ok()
            }
            "playlist.removeItem" => {
                if let Some(mut p) = self.find_playlist(&s("id")).cloned() {
                    let i = idx("index");
                    if i < p.items.len() {
                        p.items.remove(i);
                        self.update_playlist(host, &p);
                    }
                }
                ok()
            }
            "playlist.rename" => {
                if let Some(mut p) = self.find_playlist(&s("id")).cloned() {
                    p.name = s("name");
                    self.update_playlist(host, &p);
                }
                ok()
            }
            "playlist.export" => self.export_playlist(&s("id"), &s("folder")),
            "playlist.delete" => {
                self.delete_playlist(host, &s("id"));
                ok()
            }
            "playlist.play" => {
                if let Some(p) = self.find_playlist(&s("id")).cloned() {
                    match p.kind {
                        PlaylistKind::Track => self.play_tracks(host, p.items, idx("start")),
                        PlaylistKind::Background => {
                            self.bg_queue.replace(p.items, idx("start"), &mut self.rng);
                            self.show_queue_background(host);
                            self.settings_changed();
                        }
                    }
                }
                ok()
            }

            "toast" => {
                self.toast(s("text"), b("error"));
                ok()
            }

            // ---- backgrounds ----------------------------------------------
            "backgrounds.list" => {
                let cur = self.bg_queue.position();
                Value::Array(
                    self.bg_queue
                        .ordered()
                        .enumerate()
                        .map(|(pos, (_, src))| {
                            self.background_json(src)
                                .with("pos", pos)
                                .with("current", Some(pos) == cur)
                        })
                        .collect(),
                )
            }
            "backgrounds.add" => {
                let sources: Vec<String> = strs(c.get("sources"))
                    .into_iter()
                    .map(|p| {
                        if p.contains(':') && !p.starts_with('/') && !p[1..].starts_with(":\\") {
                            p
                        } else {
                            format!("{LOCAL_PREFIX}{p}")
                        }
                    })
                    .collect();
                self.add_backgrounds(host, sources, b("show"));
                ok()
            }
            "backgrounds.addFolder" => {
                #[cfg(not(target_arch = "wasm32"))]
                let images: Vec<String> = library::list_images(&s("path"))
                    .into_iter()
                    .map(|p| format!("{LOCAL_PREFIX}{p}"))
                    .collect();
                #[cfg(target_arch = "wasm32")]
                let images: Vec<String> = Vec::new();
                let n = images.len();
                self.add_backgrounds(host, images, false);
                self.toast(format!("{n} {}", self.tr("images added")), false);
                ok()
            }
            "backgrounds.select" => {
                self.bg_queue.set_position(idx("pos"));
                self.show_queue_background(host);
                self.settings_changed();
                ok()
            }
            "backgrounds.remove" => {
                self.bg_queue.remove_at(idx("pos"));
                self.show_queue_background(host);
                self.settings_changed();
                ok()
            }
            "backgrounds.clear" => {
                self.bg_queue.clear();
                self.show_queue_background(host);
                self.settings_changed();
                ok()
            }
            "backgrounds.next" => {
                self.next_background(host);
                ok()
            }
            "backgrounds.prev" => {
                self.prev_background(host);
                ok()
            }
            "backgrounds.shuffle" => {
                let on = if c.get("value").is_null() {
                    !self.bg_queue.shuffle
                } else {
                    b("value")
                };
                self.bg_queue.set_shuffle(on, &mut self.rng);
                self.revs.backgrounds += 1;
                self.settings_changed();
                ok()
            }
            "images.list" => Value::Array(
                self.server_images
                    .iter()
                    .map(|i| {
                        Value::object()
                            .with("id", i.id.as_str())
                            .with("name", i.name.as_str())
                            .with("source", format!("srv:{}", i.id))
                            .with(
                                "thumb",
                                self.server
                                    .image_url(&bl_net::api::paths::image(&i.id, 384)),
                            )
                    })
                    .collect(),
            ),
            "images.upload" => {
                let path = s("path");
                match host.fs_read(&path) {
                    Ok(data) => {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "image".into());
                        self.upload_image(host, &name, data);
                        ok()
                    }
                    Err(e) => err(e),
                }
            }

            // ---- presets and effects ----------------------------------------
            "presets.list" => Value::Array(
                self.preset_list
                    .iter()
                    .map(|p| {
                        Value::object()
                            .with("name", p.name.as_str())
                            .with("origin", p.origin.name())
                            .with("active", p.name == self.preset.name)
                    })
                    .collect(),
            ),
            "preset.load" => {
                let name = s("name");
                self.load_preset_by_name(host, &name);
                ok()
            }
            "preset.save" => {
                let name = if c.get("name").is_null() {
                    self.preset.name.clone()
                } else {
                    s("name")
                };
                let srv = if c.get("server").is_null() {
                    self.preset_origin == PresetOrigin::Server
                } else {
                    b("server")
                };
                self.save_preset_as(host, &name, srv);
                ok()
            }
            "preset.delete" => {
                self.delete_preset(host, &s("name"));
                ok()
            }
            "preset.undo" => {
                self.undo_preset();
                ok()
            }
            "preset.redo" => {
                self.redo_preset();
                ok()
            }
            "preset.import" => {
                let path = s("path");
                match host.fs_read(&path) {
                    Ok(d) => {
                        let stem = std::path::Path::new(&path)
                            .file_stem()
                            .map(|x| x.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        self.import_preset_bytes(&stem, &d);
                        ok()
                    }
                    Err(e) => err(e),
                }
            }
            "preset.export" => Value::from(self.preset.to_string_pretty()),
            "preset.effects" => {
                let shadowed = self.preset.shadowed_effects();
                Value::Array(
                    self.preset
                        .effects
                        .iter()
                        .map(|e| {
                            let spec = e.spec();
                            Value::object()
                                .with("id", e.id.as_str())
                                .with("name", e.name.as_str())
                                .with("type", e.effect_type.as_str())
                                .with("kind", e.ty.as_str())
                                .with(
                                    "typeLabel",
                                    format!("{} · {}", self.tr(&e.effect_type), self.tr(&e.ty)),
                                )
                                .with("enabled", !e.disabled)
                                .with("shadowed", shadowed.contains(&e.id))
                                .with("trigger", self.trigger_label(e))
                                .with(
                                    "description",
                                    spec.map(|s| self.tr(s.description).to_string())
                                        .unwrap_or_default(),
                                )
                        })
                        .collect(),
                )
            }
            "effect.types" => Value::Array(
                schema::EFFECTS
                    .iter()
                    .map(|s| {
                        Value::object()
                            .with("type", s.effect_type)
                            .with("kind", s.ty)
                            .with("label", self.tr(s.default_name))
                            .with("group", self.tr(s.effect_type))
                            .with("description", self.tr(s.description))
                    })
                    .collect(),
            ),
            "effect.add" => {
                let Some(spec) = schema::find(&s("type"), &s("kind")) else {
                    return err("unknown effect type");
                };
                let before = self.preset.clone();
                let e = Effect::new(spec, &mut self.rng);
                let id = e.id.clone();
                self.preset.effects.push(e);
                self.preset_edited(before);
                Value::object().with("id", id)
            }
            "effect.remove" | "effect.duplicate" | "effect.moveUp" | "effect.moveDown"
            | "effect.toggle" | "effect.rename" => {
                let id = s("id");
                let Some(i) = self.preset.effects.iter().position(|e| e.id == id) else {
                    return err("unknown effect");
                };
                let before = self.preset.clone();
                match name.as_str() {
                    "effect.remove" => {
                        self.preset.effects.remove(i);
                    }
                    "effect.duplicate" => {
                        let mut e = self.preset.effects[i].clone();
                        e.id = self.rng.id();
                        e.name = format!("{} {}", e.name, self.tr("(copy)"));
                        self.preset.effects.insert(i + 1, e);
                    }
                    "effect.moveUp" if i > 0 => self.preset.effects.swap(i, i - 1),
                    "effect.moveDown" if i + 1 < self.preset.effects.len() => {
                        self.preset.effects.swap(i, i + 1)
                    }
                    "effect.toggle" => {
                        let e = &mut self.preset.effects[i];
                        e.disabled = if c.get("enabled").is_null() {
                            !e.disabled
                        } else {
                            !b("enabled")
                        };
                    }
                    "effect.rename" => self.preset.effects[i].name = s("name"),
                    _ => return ok(),
                }
                if name == "effect.rename" {
                    self.preset_edited_coalesced(before, &format!("{id}/name"));
                } else {
                    self.preset_edited(before);
                }
                ok()
            }
            "effect.form" => self.effect_form(&s("id")),
            "effect.set" => {
                let (id, block, key) = (s("id"), s("block"), s("key"));
                let v = c.get("value").clone();
                let before = self.preset.clone();
                let Some(e) = self.preset.find_effect_mut(&id) else {
                    return err("unknown effect");
                };
                match block.as_str() {
                    "config" => e.config.set(key.as_str(), v),
                    "trigger" => e.trigger.set(key.as_str(), v),
                    "camera" => e.camera.set(key.as_str(), v),
                    b if b.starts_with("camera.") => {
                        let sub = b.trim_start_matches("camera.");
                        let mut o = e.camera.get(sub).clone();
                        o.set(key.as_str(), v);
                        e.camera.set(sub, o);
                    }
                    _ => return err("unknown block"),
                }
                e.normalize();
                self.preset_edited_coalesced(before, &format!("{id}/{block}/{key}"));
                ok()
            }
            "analysis.form" => self.analysis_form(),
            "analysis.set" => {
                let (block, key) = (s("block"), s("key"));
                let v = c.get("value").clone();
                let before = self.preset.clone();
                if block == "analysis" {
                    self.preset.analysis.set(key.as_str(), v);
                } else {
                    let mut o = self.preset.novelty.get(&block).clone();
                    o.set(key.as_str(), v);
                    self.preset.novelty.set(block.as_str(), o);
                }
                self.preset.normalize();
                self.preset_edited_coalesced(before, &format!("analysis/{block}/{key}"));
                ok()
            }
            "analysis.reset" => {
                let before = self.preset.clone();
                self.preset.analysis = Value::Null;
                self.preset.novelty = Value::Null;
                self.preset.normalize();
                self.preset_edited(before);
                ok()
            }
            "analysis.curves" => {
                self.analysis_curves(host, n("window").max(1.0), idx("columns").clamp(16, 2000))
            }

            // ---- settings ----------------------------------------------------
            "settings.get" => self.settings_json(),
            "settings.set" => {
                self.set_setting(host, &s("key"), c.get("value").clone());
                ok()
            }

            // ---- server ------------------------------------------------------
            "server.connect" => {
                let url = s("url").trim().trim_end_matches('/').to_string();
                let url = if !url.is_empty() && !url.contains("://") {
                    format!("http://{url}")
                } else {
                    url
                };
                if url != self.server.base {
                    self.vault_lock(host);
                    self.server.token.clear();
                    self.settings.server_token.clear();
                    self.settings.server_cert_pin.clear();
                }
                self.server.base = url.clone();
                self.settings.server_url = url;
                self.settings.use_server = !self.server.base.is_empty();
                self.settings_changed();
                self.connect_server(host);
                ok()
            }
            // Trusts the server's (self-signed) certificate with this
            // fingerprint, after the user compared it.
            "server.trustCertificate" => {
                let f = s("fingerprint").to_ascii_lowercase();
                if f.len() != 64 || !f.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return err("invalid fingerprint".to_string());
                }
                self.settings.server_cert_pin = f;
                self.settings_changed();
                self.connect_server(host);
                ok()
            }
            "server.login" => {
                let pw = s("password");
                self.server_login(host, &pw);
                ok()
            }
            "server.logout" => {
                self.server.token.clear();
                self.settings.server_token.clear();
                self.settings_changed();
                self.connect_server(host);
                ok()
            }
            // Checks the server's web app against the signed manifest.
            "server.verifyWeb" => {
                self.webcheck_start(host);
                ok()
            }
            "server.disconnect" => {
                self.vault_lock(host);
                self.settings.use_server = false;
                self.server.state = crate::server::ServerState::Disabled;
                self.library.set_remote(Default::default());
                self.server_playlists.clear();
                self.server_images.clear();
                self.server_presets.clear();
                self.refresh_preset_list(host);
                self.revs.playlists += 1;
                self.revs.images += 1;
                self.settings_changed();
                ok()
            }

            // ---- visualizer / window ----------------------------------------
            "ui.blur" => {
                self.blur_target = n("value").clamp(0.0, 4.0) as f32;
                ok()
            }
            "ui.overlay" => {
                self.overlay.rect = [n("x") as f32, n("y") as f32, n("w") as f32, n("h") as f32];
                self.overlay.alpha = n("alpha").clamp(0.0, 1.0) as f32;
                ok()
            }
            // The UI reports whether it is on screen (battery saving).
            "app.visible" => {
                self.visible = b("visible");
                ok()
            }
            "net.metered" => {
                self.metered = b("metered");
                ok()
            }
            "save" => {
                self.save_settings(host);
                ok()
            }
            n if n.starts_with("vault.") => self.vault_command(host, n, c),
            n if n.starts_with("outputs.") => self.outputs_command(host, n, c),
            n if n.starts_with("extensions.") => self.extensions_command(host, n, c),
            n if n.starts_with("capture.") => self
                .capture_command(host, n, c)
                .unwrap_or_else(|| err(format!("unknown command {name}"))),
            n if n.starts_with("push.") => self.push_command(host, n, c),
            n if n.starts_with("mic.") => self.mic_command(host, n, c),
            _ => err(format!("unknown command {name}")),
        }
    }

    pub(crate) fn set_setting(&mut self, host: &mut dyn Host, key: &str, value: Value) {
        if let Some(sub) = key.strip_prefix("ui.") {
            self.settings.ui.set(sub, value);
            self.settings_changed();
            return;
        }
        let mut v = self.settings.to_json();
        v.set(key, value);
        let new = Settings::from_json(&v);
        let old = std::mem::replace(&mut self.settings, new);
        if key.starts_with("airplay") || key.starts_with("dlna") {
            // Retry starting the receivers with the new settings.
            self.airplay.error = None;
            self.dlna.error = None;
        }
        if old.notifications != self.settings.notifications {
            if self.settings.notifications {
                if self.vault_unlocked() {
                    self.push_start(host);
                }
            } else {
                self.push_stop(host);
            }
        }
        if (old.volume - self.settings.volume).abs() > 1e-6 || old.muted != self.settings.muted {
            self.player.set_volume(if self.settings.muted {
                0.0
            } else {
                self.settings.volume
            });
        }
        if old.intensity != self.settings.intensity {
            self.engine.set_intensity(self.settings.intensity);
        }
        if old.adaptive != self.settings.adaptive {
            self.engine.set_adaptive(self.settings.adaptive);
        }
        if old.normalize != self.settings.normalize {
            self.apply_normalization(host);
        }
        if old.crossfade != self.settings.crossfade
            || old.skip_silence != self.settings.skip_silence
        {
            self.player
                .set_transition(self.settings.crossfade, self.settings.skip_silence);
        }
        if old.library_folders != self.settings.library_folders {
            self.rescan_local();
        }
        if old.cover_fallback != self.settings.cover_fallback {
            self.show_queue_background(host);
        }
        self.settings_changed();
    }

    fn trigger_label(&self, e: &Effect) -> String {
        let t = e.trigger_type();
        if t.is_empty() || t == "None" {
            match e.spec().map(|s| s.trigger) {
                Some(TriggerKind::None) | None => String::new(),
                _ => self.tr("No trigger").to_string(),
            }
        } else {
            self.tr(t).to_string()
        }
    }

    fn effect_form(&self, id: &str) -> Value {
        let Some(e) = self.preset.find_effect(id) else {
            return err("unknown effect");
        };
        let Some(spec) = e.spec() else {
            return err("unknown effect type");
        };
        let tr = self.trf();
        let mut sections = vec![section(
            format!("{} · {}", tr(spec.effect_type), tr(spec.ty)),
            "config",
            spec.config,
            &e.config,
            &tr,
        )];
        match spec.trigger {
            TriggerKind::Novelty => {
                sections.push(section(
                    tr("Trigger"),
                    "trigger",
                    schema::NOVELTY_TRIGGER,
                    &e.trigger,
                    &tr,
                ));
                if !spec.targets.is_empty() {
                    sections.push(section(
                        tr("Trigger animation (target values)"),
                        "trigger",
                        spec.targets,
                        &e.trigger,
                        &tr,
                    ));
                }
            }
            TriggerKind::Transition => sections.push(section(
                tr("Trigger"),
                "trigger",
                schema::TRANSITION_TRIGGER,
                &e.trigger,
                &tr,
            )),
            TriggerKind::None => {}
        }
        if spec.camera {
            sections.push(section(
                tr("Camera"),
                "camera",
                &[schema::CAMERA_SPEED],
                &e.camera,
                &tr,
            ));
            for (block, title, fields) in [
                (
                    "camera.exit",
                    "Camera exit (outgoing image)",
                    schema::CAMERA_EXIT,
                ),
                (
                    "camera.enter",
                    "Camera enter (incoming image)",
                    schema::CAMERA_ENTER,
                ),
                (
                    "camera.finish",
                    "Camera finish (incoming image)",
                    schema::CAMERA_FINISH,
                ),
            ] {
                let sub = block.trim_start_matches("camera.");
                sections.push(section(tr(title), block, fields, e.camera.get(sub), &tr));
            }
        }
        Value::object()
            .with("id", e.id.as_str())
            .with("name", e.name.as_str())
            .with("enabled", !e.disabled)
            .with("description", tr(spec.description))
            .with("sections", Value::Array(sections))
    }

    fn analysis_form(&self) -> Value {
        let tr = self.trf();
        let mut sections = vec![section(
            tr("Structure (transitions)"),
            "analysis",
            schema::ANALYSIS,
            &self.preset.analysis,
            &tr,
        )];
        for band in ["Low", "Mid", "High"] {
            sections.push(section(
                format!("{} – {}", tr("Novelty"), tr(band)),
                band,
                schema::NOVELTY,
                self.preset.novelty.get(band),
                &tr,
            ));
        }
        Value::Array(sections)
    }

    /// Downsampled analysis curves around the playback position for the
    /// analysis graphs.
    fn analysis_curves(&self, host: &dyn Host, window: f64, cols: usize) -> Value {
        let Some(a) = &self.analysis else {
            return Value::Null;
        };
        let pos = self.player.position(host);
        let start = (pos - window * 0.2).max(0.0);
        let rate = a.frame_rate();
        let sample = |data: &[f32], rate: f64| -> Vec<Value> {
            (0..cols)
                .map(|c| {
                    let t0 = start + window * c as f64 / cols as f64;
                    let t1 = start + window * (c + 1) as f64 / cols as f64;
                    let i0 = (t0 * rate) as usize;
                    let i1 = ((t1 * rate) as usize).max(i0 + 1).min(data.len());
                    if i0 >= data.len() {
                        return Value::from(0.0);
                    }
                    Value::from(data[i0..i1].iter().cloned().fold(0.0f32, f32::max))
                })
                .collect()
        };
        let trate = rate / a.transition_down_sampling.max(1) as f64;
        Value::object()
            .with("start", start)
            .with("window", window)
            .with("position", pos)
            .with("energy", Value::Array(sample(&a.energy, rate)))
            .with(
                "novelty",
                Value::Array(
                    a.novelty
                        .iter()
                        .take(3)
                        .map(|c| Value::Array(sample(c, rate)))
                        .collect(),
                ),
            )
            .with("transitions", Value::Array(sample(&a.transitions, trate)))
    }

    fn background_json(&self, src: &str) -> Value {
        let (label, thumb) = if src == DEFAULT_BG {
            (self.tr("Default background").to_string(), String::new())
        } else if let Some(p) = src.strip_prefix(LOCAL_PREFIX) {
            let name = std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.to_string());
            (name, crate::core::file_url(p))
        } else if let Some(id) = src.strip_prefix("srv:") {
            let name = self
                .server_images
                .iter()
                .find(|i| i.id == id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| id.to_string());
            (
                name,
                self.server.image_url(&bl_net::api::paths::image(id, 384)),
            )
        } else if let Some(id) = src.strip_prefix(crate::vault::BG_PREFIX) {
            self.vault_background_info(id)
        } else if let Some(tid) = src.strip_prefix("cover:") {
            let t = self.track_for(tid);
            (t.title.clone(), self.cover_url_cached(&t))
        } else {
            (src.trim_start_matches("mem:").to_string(), String::new())
        };
        Value::object()
            .with("source", src)
            .with("label", label)
            .with("thumb", thumb)
            .with("active", self.bg_source.as_deref() == Some(src))
    }

    /// Folder view of the library: sub folders and tracks of `path` (""
    /// lists the library folders and, when connected, the server).
    fn folder_view(&self, path: &str) -> Value {
        let mut folders: Vec<(String, String, usize)> = Vec::new();
        let mut tracks = Vec::new();
        if path.is_empty() {
            for f in &self.settings.library_folders {
                let n = self
                    .library
                    .tracks
                    .iter()
                    .filter(|t| library::is_local(&t.id) && in_folder(&t.path, f))
                    .count();
                let name = std::path::Path::new(f)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| f.clone());
                folders.push((name, f.clone(), n));
            }
            if !self.library.remote.tracks.is_empty() {
                folders.push((
                    self.tr("Server").to_string(),
                    "server:".to_string(),
                    self.library.remote.tracks.len(),
                ));
            }
        } else {
            // Local paths may mix "/" and "\" on Windows (a folder picked as
            // "C:/Music" scanned into "C:/Music\Rock\a.mp3"): compare and
            // list them with "/" only.
            let (is_server, prefix) = match path.strip_prefix("server:") {
                Some(p) => (true, p.trim_end_matches('/').to_string()),
                None => (false, folder_key(path)),
            };
            let mut sub: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for t in &self.library.tracks {
                if library::is_local(&t.id) == is_server {
                    continue;
                }
                let full = if is_server {
                    std::borrow::Cow::Borrowed(t.path.as_str())
                } else {
                    std::borrow::Cow::Owned(t.path.replace('\\', "/"))
                };
                let rest = if prefix.is_empty() {
                    Some(full.as_ref())
                } else {
                    full.strip_prefix(prefix.as_str())
                        .and_then(|r| r.strip_prefix('/'))
                };
                let Some(rest) = rest else { continue };
                match rest.split_once('/') {
                    Some((dir, _)) => *sub.entry(dir.to_string()).or_default() += 1,
                    None => tracks.push(self.track_json(t)),
                }
            }
            for (name, count) in sub {
                let full = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                let full = if is_server {
                    format!("server:{full}")
                } else {
                    full
                };
                folders.push((name, full, count));
            }
        }
        Value::object()
            .with("path", path)
            .with(
                "folders",
                Value::Array(
                    folders
                        .into_iter()
                        .map(|(name, p, count)| {
                            Value::object()
                                .with("name", name)
                                .with("path", p)
                                .with("count", count)
                        })
                        .collect(),
                ),
            )
            .with("tracks", Value::Array(tracks))
    }

    pub(crate) fn find_playlist(&self, id: &str) -> Option<&Playlist> {
        self.local_playlists
            .iter()
            .chain(self.server_playlists.iter())
            .find(|p| p.id == id)
    }

    pub(crate) fn create_playlist(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        kind: PlaylistKind,
        items: Vec<String>,
        on_server: bool,
    ) {
        let name = if name.trim().is_empty() {
            self.tr("New playlist").to_string()
        } else {
            name.trim().to_string()
        };
        if on_server && self.server.is_connected() {
            let body = Value::object()
                .with("name", name.as_str())
                .with("kind", kind.name())
                .to_json()
                .into_bytes();
            // Create, then (if items) update via a second request once we know the id.
            let id = self.server.send(
                host,
                "POST",
                bl_net::api::paths::PLAYLISTS,
                body,
                "application/json",
            );
            self.requests.insert(id, Req::PlaylistSaved);
            if !items.is_empty() {
                self.pending_playlist_items = Some((name, items));
            }
        } else {
            let id = format!("local-{:016x}", self.rng.next_u64());
            self.local_playlists.push(Playlist {
                id,
                name,
                kind,
                items,
                modified: host.unix_time() as u64,
            });
            self.save_local_playlists(host);
        }
    }

    pub(crate) fn update_playlist(&mut self, host: &mut dyn Host, pl: &Playlist) {
        if pl.id.starts_with("local-") {
            if let Some(p) = self.local_playlists.iter_mut().find(|p| p.id == pl.id) {
                *p = pl.clone();
            }
            self.save_local_playlists(host);
        } else {
            let body = Value::object()
                .with("name", pl.name.as_str())
                .with("items", pl.items.clone())
                .to_json()
                .into_bytes();
            let id = self.server.send(
                host,
                "PUT",
                &bl_net::api::paths::playlist(&pl.id),
                body,
                "application/json",
            );
            self.requests.insert(id, Req::PlaylistSaved);
            if let Some(p) = self.server_playlists.iter_mut().find(|p| p.id == pl.id) {
                *p = pl.clone();
            }
            self.revs.playlists += 1;
        }
    }

    pub(crate) fn delete_playlist(&mut self, host: &mut dyn Host, id: &str) {
        if id.starts_with("local-") {
            self.local_playlists.retain(|p| p.id != id);
            self.save_local_playlists(host);
        } else {
            let rid = self.server.send(
                host,
                "DELETE",
                &bl_net::api::paths::playlist(id),
                Vec::new(),
                "application/json",
            );
            self.requests.insert(rid, Req::PlaylistSaved);
            self.server_playlists.retain(|p| p.id != id);
            self.revs.playlists += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_with_mixed_separators() {
        assert_eq!(folder_key("C:\\Music\\"), "C:/Music");
        assert!(in_folder("C:/Music\\Rock\\a.mp3", "C:/Music"));
        assert!(in_folder("C:\\Music\\a.mp3", "C:/Music/"));
        assert!(!in_folder("C:/Music2/a.mp3", "C:/Music"));
        assert!(in_folder("/a.mp3", "/"));
    }
}
