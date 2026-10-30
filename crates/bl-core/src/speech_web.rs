//! Speech recognition in the browser: not available (the server can do it).

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;

#[derive(Default)]
pub(crate) struct SpeechState {
    pub(crate) rev: u64,
}

impl Core {
    pub(crate) fn speech_json(&self, _host: &dyn Host) -> Value {
        Value::object()
            .with("available", false)
            .with("rev", self.speech.rev)
    }

    pub(crate) fn speech_command(
        &mut self,
        _host: &mut dyn Host,
        _name: &str,
        _c: &Value,
    ) -> Value {
        Value::object().with("error", "not available in the browser")
    }

    pub(crate) fn poll_speech(&mut self, _host: &mut dyn Host) {}
}
