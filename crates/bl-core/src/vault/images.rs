//! Images from the vault as backgrounds (`vault:<file id>`). They are
//! decrypted in memory only; thumbnails for choosing one are made from the
//! decrypted image and forgotten when the vault locks.

use super::{Done, VaultFile, VaultReq};
use crate::core::Core;
use crate::host::Host;
use bl_json::Value;
use std::sync::Arc;

pub(crate) const BG_PREFIX: &str = "vault:";
const THUMB_SIZE: u32 = 320;

pub(super) fn is_image(f: &VaultFile) -> bool {
    f.mime.starts_with("image/")
        || matches!(
            crate::jobs::extension(&f.name).as_deref(),
            Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp")
        )
}

/// A small JPEG of the image, as a data: URL for the image list.
pub(super) fn thumbnail(data: &[u8]) -> Option<String> {
    let img = std::panic::catch_unwind(|| bl_image::decode_max(data, THUMB_SIZE * 2).ok())
        .ok()
        .flatten()?;
    let scale = (THUMB_SIZE as f32 / img.width.max(img.height).max(1) as f32).min(1.0);
    let (w, h) = (
        ((img.width as f32 * scale) as u32).max(1),
        ((img.height as f32 * scale) as u32).max(1),
    );
    let small = bl_image::resize(&img, w, h);
    Some(format!(
        "data:image/jpeg;base64,{}",
        bl_net::base64_encode(&bl_image::encode_jpeg(&small, 80))
    ))
}

impl Core {
    /// The vault's images, for choosing backgrounds (`vault.images`).
    pub(crate) fn vault_images(&mut self, host: &mut dyn Host) -> Value {
        let images: Vec<(String, String)> = self
            .vault
            .files
            .iter()
            .filter(|f| f.complete && f.key.is_some() && is_image(f))
            .map(|f| (f.id.clone(), f.name.clone()))
            .collect();
        let mut out = Vec::new();
        for (id, name) in images {
            let thumb = self.vault.thumbs.get(&id).cloned();
            if thumb.is_none() && self.vault.thumbs_pending.insert(id.clone()) {
                self.vault_fetch_image(host, &id, true);
            }
            out.push(
                Value::object()
                    .with("id", id.as_str())
                    .with("name", name)
                    .with("source", format!("{BG_PREFIX}{id}"))
                    .with("thumb", thumb.unwrap_or_default()),
            );
        }
        Value::Array(out)
    }

    /// Label and thumbnail of a `vault:` background.
    pub(crate) fn vault_background_info(&self, id: &str) -> (String, String) {
        let name = self
            .vault
            .file(id)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| self.tr("Encrypted image").to_string());
        (name, self.vault.thumbs.get(id).cloned().unwrap_or_default())
    }

    /// Shows a `vault:` background; false when it cannot be opened (yet).
    pub(crate) fn vault_load_background(&mut self, host: &mut dyn Host, id: &str) -> bool {
        if self.vault.file(id).and_then(|f| f.key.as_ref()).is_none() {
            return false;
        }
        if let Some(data) = self.vault.plain(id) {
            let animate = self.bg_source.is_some();
            self.decode_background_async(host, format!("{BG_PREFIX}{id}"), data.to_vec(), animate);
        } else {
            self.vault_fetch_image(host, id, false);
        }
        true
    }

    fn vault_fetch_image(&mut self, host: &mut dyn Host, id: &str, thumb: bool) {
        let path = format!("/api/vault/files/{id}/data");
        self.vault_get(
            host,
            &path,
            VaultReq::Image {
                id: id.to_string(),
                thumb,
            },
        );
    }

    /// The encrypted image arrived.
    pub(super) fn vault_image_data(
        &mut self,
        host: &mut dyn Host,
        id: String,
        thumb: bool,
        body: Option<Vec<u8>>,
    ) {
        let plain = self.vault.file(&id).and_then(|f| {
            let key = f.key.clone()?;
            bl_vault::file::decrypt(&key, &body?).ok()
        });
        let Some(data) = plain else {
            self.vault.thumbs_pending.remove(&id);
            if !thumb && self.bg_pending.as_deref() == Some(&format!("{BG_PREFIX}{id}")) {
                self.bg_pending = None;
                self.toast(self.tr("Could not load background image"), true);
            }
            return;
        };
        let data = Arc::new(data);
        if thumb {
            let d = data.clone();
            self.vault_job(host, move || Done::Thumb {
                url: thumbnail(&d).unwrap_or_default(),
                id,
            });
        } else if self.bg_pending.as_deref() == Some(&format!("{BG_PREFIX}{id}")) {
            let animate = self.bg_source.is_some();
            self.decode_background_async(host, format!("{BG_PREFIX}{id}"), data.to_vec(), animate);
            self.vault.remember_plain(&id, data);
        }
    }

    /// After the file list changed: a `vault:` background waiting for it.
    pub(super) fn vault_backgrounds_ready(&mut self, host: &mut dyn Host) {
        let current = self.bg_queue.current().map(String::as_str);
        let waiting = current.is_some_and(|s| s.starts_with(BG_PREFIX))
            && self.bg_source.as_deref() != current
            && self.bg_pending.as_deref() != current;
        if waiting {
            self.bg_pending = None;
            self.show_queue_background(host);
        }
        self.revs.backgrounds += 1;
    }

    /// Locking: a decrypted background is not shown any longer.
    pub(super) fn vault_backgrounds_locked(&mut self, host: &mut dyn Host) {
        if self
            .bg_source
            .as_deref()
            .is_some_and(|s| s.starts_with(BG_PREFIX))
        {
            self.bg_source = None;
            self.bg_pending = None;
            self.load_background(host, crate::core::DEFAULT_BG);
        }
        self.revs.backgrounds += 1;
        self.revs.images += 1;
    }
}
