//! The alignment models to download: wav2vec2 models fine-tuned for
//! speech recognition with CTC, as published on Hugging Face (Apache-2.0),
//! with the files the aligner reads (name, size, SHA-256).
//!
//! English has a small model (95 M parameters, about 190 MB in memory as
//! f16); the other languages have the large XLSR-53 models of Jonatas
//! Grosman (315 M parameters, about 630 MB in memory as f16). Pass
//! [`AlignModel::specs`] to `bl_asr::models::Models::download_files`.

use bl_asr::models::FileSpec;

/// A model for one language.
pub struct AlignModel {
    /// Folder name under the models folder.
    pub id: &'static str,
    /// ISO 639-1 code of the language.
    pub language: &'static str,
    /// Hugging Face repository.
    pub repo: &'static str,
    /// (name, size, SHA-256) of the files needed.
    pub files: &'static [(&'static str, u64, &'static str)],
}

impl AlignModel {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }

    /// The files to download.
    pub fn specs(&self) -> Vec<FileSpec> {
        self.files
            .iter()
            .map(|(name, size, sha)| FileSpec {
                name: name.to_string(),
                url: format!("https://huggingface.co/{}/resolve/main/{name}", self.repo),
                size: *size,
                sha256: sha.to_string(),
            })
            .collect()
    }
}

/// The preprocessor settings shared by the XLSR models.
const XLSR_PREPROCESSOR: (&str, u64, &str) = (
    "preprocessor_config.json",
    262,
    "ca5999a45e98bb76ea87a461ba28a23ad32a5bb9f733b8e0f6546ff38b6c612d",
);

pub const CATALOG: &[AlignModel] = &[
    AlignModel {
        id: "wav2vec2-base-960h",
        language: "en",
        repo: "facebook/wav2vec2-base-960h",
        files: &[
            (
                "pytorch_model.bin",
                377_667_514,
                "c34f9827b034a1b9141dbf6f652f8a60eda61cdf5771c9e05bfa99033c92cd96",
            ),
            (
                "config.json",
                1596,
                "d3ec255c063d9f95057b553b19c20135b259875834a4fe9deb218a6be25b4cf3",
            ),
            (
                "vocab.json",
                291,
                "19727f8944fe6459fc3f240ae2c198395b740f6a029bd23e06656266b83bcf64",
            ),
            (
                "preprocessor_config.json",
                159,
                "b225d617c025463b9e157e06afea8b90dc7078fc70b013c533328423e0486b4a",
            ),
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-german",
        language: "de",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-german",
        files: &[
            (
                "pytorch_model.bin",
                1_262_089_623,
                "bff6d75ab89d8ca9cd103df9beb9c10f547501cf5a34aeabea1c8d736c1b81cb",
            ),
            (
                "config.json",
                1782,
                "42175a918795a7343c58dbcad995603912a0d85c729cc4d7b8eb089a7a569183",
            ),
            (
                "vocab.json",
                350,
                "31d0914432f4492a24ce83abfd767421844afe5cef20346a48c6c877dee28378",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-french",
        language: "fr",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-french",
        files: &[
            (
                "pytorch_model.bin",
                1_262_175_703,
                "2fc4760bab7bf7d0d6c06e9b7f829a27a3e66eefee3733f53c88dbedf6607f2e",
            ),
            (
                "config.json",
                1531,
                "ee87cfb5a4ad72d24b79eb26ab4b7c26ecc2adc1c0e0e6bc6c91e551d884ef43",
            ),
            (
                "vocab.json",
                560,
                "e83a3c849e480757fabd9fe927b870581e926407bcc7e1b6dcc8bda1e78a8d78",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-spanish",
        language: "es",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-spanish",
        files: &[
            (
                "pytorch_model.bin",
                1_262_101_911,
                "41c110e55d2eac8c79486ad87dbe8f9527ed034fe087a6adf03c891eeba914c1",
            ),
            (
                "config.json",
                1531,
                "39ffe954848d5161270eef4378a17a538bffeefc310482f409ec099f7fceaa5b",
            ),
            (
                "vocab.json",
                380,
                "7aea8bcd8ba1c176f12cc96c1e8f45247548f4de3218106f7352e3d833cfafdc",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-italian",
        language: "it",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-italian",
        files: &[
            (
                "pytorch_model.bin",
                1_262_114_199,
                "14040a54d1e0caecd8b2271fdd2d96653be45702c7bc8765354140d864fb445a",
            ),
            (
                "config.json",
                1782,
                "7e69e919b00b2044006c69cea10a0b15d35d65b6acec9f7180afef2edc7df1fb",
            ),
            (
                "vocab.json",
                410,
                "deee9194b91e4e4f628ff37da6226a9c9cfe564810c917ae545920458723c58e",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-dutch",
        language: "nl",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-dutch",
        files: &[
            (
                "pytorch_model.bin",
                1_262_093_720,
                "f307fd2a7a323dbc4dc6fda728e97d51c0be50fed198cb4d1a14d275acb255cd",
            ),
            (
                "config.json",
                1782,
                "9f02bcac826a79be1bc20cff20fb2ed830f3538ec282c65050f9195d61499deb",
            ),
            (
                "vocab.json",
                360,
                "82f746bb960adcf0324ab04fdcc97b4e56f22b75559fbcaf04845fffbe4d46c9",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-portuguese",
        language: "pt",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-portuguese",
        files: &[
            (
                "pytorch_model.bin",
                1_262_122_456,
                "c244caf8395a0c333bdc877e7b62ce5efea12ad8cfec0851c0fe6efb844b834e",
            ),
            (
                "config.json",
                1777,
                "ea1d581f043b854671b11d6b7db606e1ec5aafbfdd6b964c3f2f5975e335761c",
            ),
            (
                "vocab.json",
                430,
                "66b0813833e1536fb028f675e10ceda8254e6ee400d8cfd48359f39982190696",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-russian",
        language: "ru",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-russian",
        files: &[
            (
                "pytorch_model.bin",
                1_262_093_719,
                "d1cdb1a7921de7d363f967a9b0101a713602e109dba62b6f3f9ae2e0b2df0c1c",
            ),
            (
                "config.json",
                1782,
                "9f02bcac826a79be1bc20cff20fb2ed830f3538ec282c65050f9195d61499deb",
            ),
            (
                "vocab.json",
                387,
                "3def5e6b6c6fcbe9a674e58b8f7f9e608742414db0188ec50bb3ac565612f0d7",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
    AlignModel {
        id: "wav2vec2-large-xlsr-53-polish",
        language: "pl",
        repo: "jonatasgrosman/wav2vec2-large-xlsr-53-polish",
        files: &[
            (
                "pytorch_model.bin",
                1_262_097_815,
                "1bd2a29f3b6093cc7a0f9ad39d56b5b11ae00f588878035e117e379dd3c204a6",
            ),
            (
                "config.json",
                1782,
                "819dac7fa3ad7f4c11c15c2ede5f323effc5bcfaa311645a250392b65524c29c",
            ),
            (
                "vocab.json",
                372,
                "bf4603ae2fc39fa1e10a633b533c7535d7d6ffeb21181b6200c403264569cd0e",
            ),
            XLSR_PREPROCESSOR,
        ],
    },
];

/// The model for a language (ISO 639-1 code, e.g. `de`).
pub fn for_language(language: &str) -> Option<&'static AlignModel> {
    let l = language.get(..2).unwrap_or(language).to_ascii_lowercase();
    CATALOG.iter().find(|m| m.language == l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_complete() {
        for m in CATALOG {
            let names: Vec<&str> = m.files.iter().map(|f| f.0).collect();
            for n in [
                "pytorch_model.bin",
                "config.json",
                "vocab.json",
                "preprocessor_config.json",
            ] {
                assert!(names.contains(&n), "{} lacks {n}", m.id);
            }
            for f in m.files {
                assert_eq!(f.2.len(), 64);
                assert!(f.2.bytes().all(|b| b.is_ascii_hexdigit()));
            }
            assert!(m.repo.ends_with(m.id));
            assert!(m.specs()[0].url.starts_with("https://huggingface.co/"));
        }
        assert_eq!(
            for_language("de-AT").map(|m| m.id),
            Some("wav2vec2-large-xlsr-53-german")
        );
        assert_eq!(for_language("EN").map(|m| m.language), Some("en"));
        assert!(for_language("xx").is_none());
    }
}
