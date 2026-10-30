//! Translating song texts in the browser: not available.

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;

#[derive(Default, Clone, Copy)]
pub(crate) struct TranslateState;

impl Core {
    pub(crate) fn translate_json(&self) -> Value {
        let TranslateState = self.translate;
        Value::object().with("available", false)
    }

    pub(crate) fn translated_lines(&self, _id: &str, _n: usize) -> Option<&[String]> {
        None
    }

    pub(crate) fn translate_command(
        &mut self,
        _host: &mut dyn Host,
        _name: &str,
        _c: &Value,
    ) -> Value {
        Value::object().with("error", "not available in the browser")
    }

    pub(crate) fn poll_translate(&mut self, _host: &mut dyn Host) {}
}
