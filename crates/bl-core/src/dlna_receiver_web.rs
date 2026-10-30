//! The browser has no sockets for a DLNA renderer: nothing to do.

use crate::core::Core;
use crate::host::Host;
use crate::player::SourceOpener;
use bl_json::Value;

pub(crate) const ID_PREFIX: &str = "dlna:";

#[derive(Default)]
pub(crate) struct DlnaState {
    pub(crate) error: Option<String>,
}

impl DlnaState {
    pub(crate) fn cover_url(&self) -> &str {
        ""
    }
}

impl Core {
    pub(crate) fn playing_dlna(&self) -> bool {
        false
    }
    pub(crate) fn update_dlna(&mut self, _host: &mut dyn Host) {}
    pub(crate) fn dlna_track_ended(&mut self, _host: &mut dyn Host) {}
    pub(crate) fn end_dlna_playback(&mut self, _host: &mut dyn Host) {}
    pub(crate) fn dlna_analysable(&self, _host: &dyn Host) -> bool {
        false
    }
    pub(crate) fn dlna_opener(&self) -> Option<SourceOpener> {
        None
    }
    pub(crate) fn dlna_answer(&mut self, _host: &mut dyn Host, _allow: bool, _always: bool) {}
    pub(crate) fn dlna_json(&self) -> Value {
        Value::object().with("running", false)
    }
}
