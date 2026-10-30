//! Music sources in the browser: not available (the browser cannot make
//! the connections); the server's sources still work through the server.

use crate::core::Core;
use crate::host::Host;
use crate::player::SourceOpener;
use bl_json::Value;

pub(crate) const ID_PREFIX: &str = "src:";
pub(crate) const RADIO_PREFIX: &str = "radio:";

#[derive(Default)]
pub(crate) struct SourcesState {
    pub(crate) rev: u64,
}

impl Core {
    pub(crate) fn load_sources(&mut self, _host: &mut dyn Host) {}

    pub(crate) fn sources_secret(&mut self, _host: &mut dyn Host, _value: &str) {}

    pub(crate) fn poll_sources(&mut self, _host: &mut dyn Host) {}

    pub(crate) fn source_opener(&self, _id: &str) -> Option<SourceOpener> {
        None
    }

    pub(crate) fn radio_track(&self, _id: &str) -> Option<bl_net::api::Track> {
        None
    }

    pub(crate) fn mark_public_only(&mut self, _url: &str) {}

    pub(crate) fn play_radio(&mut self, host: &mut dyn Host, _id: &str, _autoplay: bool) {
        self.player.stop(host);
    }

    pub(crate) fn source_played(&self, _host: &mut dyn Host, _id: &str) {}

    pub(crate) fn request_source_covers(&mut self, _host: &mut dyn Host, _ids: Vec<String>) {}

    pub(crate) fn sources_json(&self) -> Value {
        Value::object()
            .with("rev", self.sources.rev)
            .with("available", false)
            .with("list", Value::Array(Vec::new()))
            .with("searching", false)
            .with("found", Value::Array(Vec::new()))
    }

    pub(crate) fn sources_command(
        &mut self,
        _host: &mut dyn Host,
        _name: &str,
        _c: &Value,
    ) -> Value {
        Value::object().with("error", "not available in the browser")
    }

    pub(crate) fn import_playlist_file(&mut self, _host: &mut dyn Host, _path: &str, _play: bool) {}

    pub(crate) fn export_playlist(&mut self, _id: &str, _folder: &str) -> Value {
        Value::object().with("error", "not available in the browser")
    }
}
