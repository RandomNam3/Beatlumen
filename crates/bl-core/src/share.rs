//! Songs, playlists and links shared with the app by other apps ("Share"
//! on Android, "Open with"): the app asks what to do with them: play now,
//! add to the queue, or keep them in the user's encrypted files (and so
//! on every device of the account).

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;

#[derive(Default)]
pub(crate) struct ShareState {
    /// Copied local files, or one link.
    paths: Vec<String>,
    link: Option<String>,
    pub(crate) rev: u64,
}

/// The first http(s) address in a shared text.
fn first_link(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | ',')))
        .find(|w| (w.starts_with("https://") || w.starts_with("http://")) && w.len() <= 2048)
        .map(str::to_string)
}

impl Core {
    pub(crate) fn share_json(&self) -> Value {
        let s = &self.share;
        if s.paths.is_empty() && s.link.is_none() {
            return Value::Null;
        }
        let names: Vec<String> = s
            .paths
            .iter()
            .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
            .collect();
        Value::object()
            .with("files", names)
            .with("link", s.link.clone())
            .with("canKeep", !s.paths.is_empty() && self.vault_ready())
    }

    /// `share.*` commands.
    pub(crate) fn share_command(&mut self, host: &mut dyn Host, name: &str, c: &Value) -> Value {
        let ok = Value::object().with("ok", true);
        match name {
            "share.receive" => {
                let paths: Vec<String> = c
                    .get("paths")
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.as_str().map(str::to_string))
                    .take(200)
                    .collect();
                let link = c.get("text").as_str().and_then(first_link);
                if paths.is_empty() && link.is_none() {
                    let msg = self.tr("Nothing to play was shared").to_string();
                    self.toast(msg, true);
                    return ok;
                }
                self.share.paths = paths;
                self.share.link = if self.share.paths.is_empty() {
                    link
                } else {
                    None
                };
                self.share.rev += 1;
            }
            "share.play" | "share.queue" => {
                let play = name == "share.play";
                let paths = std::mem::take(&mut self.share.paths);
                if let Some(url) = self.share.link.take() {
                    // A stream, a file or a station file: played live.
                    self.mark_public_only(&url);
                    let id = format!("{}{url}", crate::sources::RADIO_PREFIX);
                    if play {
                        self.play_tracks(host, vec![id], 0);
                    } else {
                        self.enqueue(host, vec![id], false);
                    }
                } else if !paths.is_empty() {
                    self.open_paths(host, paths, play);
                }
                self.share.rev += 1;
            }
            "share.keep" => {
                let paths = std::mem::take(&mut self.share.paths);
                self.share.link = None;
                self.share.rev += 1;
                if !paths.is_empty() {
                    self.vault_upload_paths(host, paths);
                }
            }
            "share.dismiss" => {
                self.share.paths.clear();
                self.share.link = None;
                self.share.rev += 1;
            }
            _ => return Value::object().with("error", "unknown command"),
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_in_shared_text() {
        assert_eq!(
            first_link("Listen: https://radio.example.org/live.mp3 (great)").as_deref(),
            Some("https://radio.example.org/live.mp3")
        );
        assert_eq!(first_link("no link here"), None);
        assert_eq!(first_link("ftp://x javascript:alert(1)"), None);
    }
}
