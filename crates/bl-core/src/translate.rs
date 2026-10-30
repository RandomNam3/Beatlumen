//! Translating song texts on the device: the OPUS-MT models of the
//! University of Helsinki (one per language pair, about 300 MB, loaded
//! from Hugging Face into the app's storage when first needed and checked
//! against their SHA-256), run by `bl_mt`. With translation on, each song
//! text is translated line by line into the app's language and kept on the
//! device; the song text view shows it under each line.

use crate::core::Core;
use crate::host::Host;
use crate::jobs::Mailbox;
use bl_asr::models::{Download, FileSpec, Models};
use bl_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Where the models come from (`<SOURCE><from>-<to>/resolve/main/<file>`).
const SOURCE: &str = "https://huggingface.co/Helsinki-NLP/opus-mt-";

/// A translation model: from one language into another, and its files
/// (name, size, SHA-256).
pub(crate) struct Pair {
    pub from: &'static str,
    pub to: &'static str,
    pub files: &'static [(&'static str, u64, &'static str)],
}

impl Pair {
    fn id(&self) -> String {
        format!("opus-mt-{}-{}", self.from, self.to)
    }

    fn size(&self) -> u64 {
        self.files.iter().map(|f| f.1).sum()
    }

    fn specs(&self) -> Vec<FileSpec> {
        self.files
            .iter()
            .map(|(name, size, sha)| FileSpec {
                name: name.to_string(),
                url: format!("{SOURCE}{}-{}/resolve/main/{name}", self.from, self.to),
                size: *size,
                sha256: sha.to_string(),
            })
            .collect()
    }
}

pub(crate) const PAIRS: &[Pair] = &[
    Pair {
        from: "en",
        to: "de",
        files: &[
            (
                "pytorch_model.bin",
                297928209,
                "da068344b1b0c20e6d4a8f77f48e06646b90d679029006b18579e68977206bdd",
            ),
            (
                "config.json",
                1335,
                "41c7890e6f6c1f72c5c626569cfe739a8362080940edbb1747f25173f541653b",
            ),
            (
                "source.spm",
                768489,
                "678f2a1177d8389f67b66299762dcc4fc567e89b07e212ba91b0c56daecf47ce",
            ),
            (
                "target.spm",
                796845,
                "bbd1f495eea99c8e21ae086d9146e0fa7b096c3dfdd9ba07ab8b631889df5c9b",
            ),
            (
                "vocab.json",
                1273232,
                "0d70d89fee4a8b4ef99a56d712163baadcabd5600a597f71515547ee70306329",
            ),
        ],
    },
    Pair {
        from: "es",
        to: "de",
        files: &[
            (
                "pytorch_model.bin",
                304494609,
                "323df13eddfaf34dfd47a6c0015cfd7ad43fcd8d0a702d2af342d8542703320f",
            ),
            (
                "config.json",
                1381,
                "600d008ffe786a28c09612853d44da8313bd9ade2f96307bd8ade92542962ebb",
            ),
            (
                "source.spm",
                818345,
                "d15fb47311fe1d385213350041a25dcb2f2b0ceb7d7102b7853b1405b33fd794",
            ),
            (
                "target.spm",
                828663,
                "4c6cf9cacbfbf82db361e3e07df76172cce7d1ef683d4e7d090349c84246caf0",
            ),
            (
                "vocab.json",
                1465772,
                "5afdba8ce02954340f89ca24f6f877b3fb89d2f4d764c43e8b99bca12f7c6b0f",
            ),
        ],
    },
    Pair {
        from: "fr",
        to: "de",
        files: &[
            (
                "pytorch_model.bin",
                304190913,
                "778b7fd12e02949ecedef012f23f9fe8736c66b2e1ef2b5bc9db2bf4c9ba7112",
            ),
            (
                "config.json",
                1381,
                "1daf893d3bd443f139898e9b2010ebe47d9a6c50a6aa36a2c3e6b1057b872154",
            ),
            (
                "source.spm",
                812399,
                "e9a9acb44b4f88235ca6cc9f0b4546f942ba34fb1b27937b11386cc8db63ee83",
            ),
            (
                "target.spm",
                827737,
                "965802053f4a59f81d48283bd1160ed49950124891f8f532d98945de4f5e58eb",
            ),
            (
                "vocab.json",
                1464136,
                "f0b50c6f6c01e5d7ad62872e61aefe1ddeb52c7740c80f0caa16cc6c0bdd129b",
            ),
        ],
    },
    Pair {
        from: "it",
        to: "de",
        files: &[
            (
                "pytorch_model.bin",
                301552041,
                "61a1d631be8f9f39472215c2c6dada2f1685c9514c7c00bec7fa928688a66009",
            ),
            (
                "config.json",
                1381,
                "78979f086b52466dffd06401861d28c0ef69cfbf3abeae6ba40e83ead3779ec7",
            ),
            (
                "source.spm",
                814042,
                "96766b8865772f73f02de781eac203a2f5206f2461b4603061ab7c6aa2aa73d7",
            ),
            (
                "target.spm",
                829341,
                "32960f04ff0fdd9c51446dabd675948b92074547cb7275551c2f2595d68b2bb0",
            ),
            (
                "vocab.json",
                1388022,
                "d595cc16f44629ca4621d014ec06da83a98616f97c2b3a0b26988e3292f2c8c4",
            ),
        ],
    },
    Pair {
        from: "de",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                297928209,
                "e743c3070f61f477cb62fe95ef2c9be2e77f3e488cb6b8030ff8a19e8295c87d",
            ),
            (
                "config.json",
                1381,
                "89368ef76ea89581025cdf605caac75b8a22af2c1a90ec57c8a5001f10537eeb",
            ),
            (
                "source.spm",
                796845,
                "bbd1f495eea99c8e21ae086d9146e0fa7b096c3dfdd9ba07ab8b631889df5c9b",
            ),
            (
                "target.spm",
                768489,
                "678f2a1177d8389f67b66299762dcc4fc567e89b07e212ba91b0c56daecf47ce",
            ),
            (
                "vocab.json",
                1273232,
                "0d70d89fee4a8b4ef99a56d712163baadcabd5600a597f71515547ee70306329",
            ),
        ],
    },
    Pair {
        from: "es",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                312087523,
                "586a4e69e0804459ac7c176a5dc76852652100d9af1ee0db796de719899a5deb",
            ),
            (
                "config.json",
                1438,
                "374443eacf8986c21386503e2fcd52eae6952ac09c87fd36695f5c12259a4cd1",
            ),
            (
                "source.spm",
                825924,
                "e236ee6d866b635c0142114f8647f39831f9d92534aa2aad75c942f6a78ad0e3",
            ),
            (
                "target.spm",
                801636,
                "4dd547c24816a335e7b0b2e63376a8f1b3cbfc671eda5ab808dd44fdadaa8791",
            ),
            (
                "vocab.json",
                1590040,
                "257f346d7a6b2ecceafcca8ba05648ce2fd68dfaf105fb0e913dca7198f3f6d5",
            ),
        ],
    },
    Pair {
        from: "fr",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                300827685,
                "599b819e3488f0fb888fef09511370ce4c0388b6f0f6beeb49a1f4b19043bebc",
            ),
            (
                "config.json",
                1416,
                "b3be13d046d9899d7aab8cf4ed624d9a79f5776038ba793f6b4d2ce3e02192f7",
            ),
            (
                "source.spm",
                802397,
                "78d0e717c77053f1c4b856d8661d9cb87c64f083a35418c087b9146300e4f585",
            ),
            (
                "target.spm",
                778395,
                "173e9f493a668fe396d599e28d414a201193094e6ffd7a4678e5aab0f6d3d838",
            ),
            (
                "vocab.json",
                1339166,
                "945c604346ce15ce4aff9001001e7f925e336d942c4087017f191871162cbdc4",
            ),
        ],
    },
    Pair {
        from: "it",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                343642677,
                "7b16287e467858b369c477d39498427197500a6b477e3d76c2fbcc54d915d8f3",
            ),
            (
                "config.json",
                1381,
                "579f74b8a178cb5d006a521e497d2de643185160d45da2df7cdf2cda65ea2861",
            ),
            (
                "source.spm",
                813709,
                "1bd307e2756991d470e4b40172147f7889b46a323b1a9a2150e66274825b1f21",
            ),
            (
                "target.spm",
                789549,
                "5d0ea845f1daa59c379e797de366607b75594935f7c6d476c933e527bf25e0a8",
            ),
            (
                "vocab.json",
                2369833,
                "f4ea61764cde265f25c2824bfe5b945685526ede81c5f2f76715a5652a63ccdb",
            ),
        ],
    },
    Pair {
        from: "nl",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                316246425,
                "70243188530be305eaee44a120fc768a1f7f52abad2f7bb4f87b70bd4fe2a6c1",
            ),
            (
                "config.json",
                1381,
                "d0e83dd5b75e1680ead94e4166b386bf901320455f0494120cbf629044fef1ba",
            ),
            (
                "source.spm",
                813866,
                "f538d23d4b30641cd97c608d2a83890d503aa1286a5560f19ef949404a786e94",
            ),
            (
                "target.spm",
                789525,
                "d05781fde84b8793191528a7a176e1b17f96265388f602379ee6ae3139f59141",
            ),
            (
                "vocab.json",
                1660216,
                "52ef60fa185ed5a6a6e75a4ea45de99582f1c1493df1c072a784489697407b2b",
            ),
        ],
    },
    Pair {
        from: "tr",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                306727185,
                "e2b820f31c9348b0ae33d77ac23bd1686fa60a8afa0b9511bc7d9a35c125b534",
            ),
            (
                "config.json",
                1381,
                "58c3d18b384cebc9383f6d85a72dae0e02cc47695097b69ff130f8be4c0e07e3",
            ),
            (
                "source.spm",
                839750,
                "4a6356e3ee892f2896dbf0b5cabeb51fdf16e03c70b0702cefc8b847e504241c",
            ),
            (
                "target.spm",
                796647,
                "4cc15427e50b80e53b7dcf5f4d8a52edfe219b8d8e290c031f5578f5760b335f",
            ),
            (
                "vocab.json",
                1563964,
                "f6518b18a71fea3de99b5883cb80bcd188b74c33e7c6a5a597b7c64119f1b1fe",
            ),
        ],
    },
    Pair {
        from: "ru",
        to: "en",
        files: &[
            (
                "pytorch_model.bin",
                306991893,
                "535450eb5613f3cc912f9ca3e54cfef6c14d201b319c24a88faf776a65538b5d",
            ),
            (
                "config.json",
                1381,
                "5ea76c78596bce8fe005ef89e00de0924bf83e5f532ce08784ff1fcefb699f5f",
            ),
            (
                "source.spm",
                1080169,
                "745998e51ba5b058e38b7ac7765c25c43ed5c1c39cc92b27163b9b2e323c9d7c",
            ),
            (
                "target.spm",
                802781,
                "16bebef1389a0b8ab452772c4e35b9e605e5713f8ac7baa71ca701394eaa086d",
            ),
            (
                "vocab.json",
                2601758,
                "33e95da3be3fa3b50169c4c46693ba2f29fbf4cb29d99044bd07d72d181fa1e9",
            ),
        ],
    },
];

type Loaded = Arc<Mutex<Option<(String, Arc<bl_mt::Translator>)>>>;

#[derive(Default)]
pub(crate) struct TranslateState {
    models: Option<Arc<Models>>,
    loaded: Loaded,
    /// Model folders whose download finished (pair ids).
    ready: Mailbox<String>,
    /// (track id, target language, lines or why not).
    results: Mailbox<(String, String, Result<Vec<String>, String>)>,
    /// The track being translated (or waiting for its model) and progress.
    job: Option<(String, Arc<AtomicU32>)>,
    waiting: Option<(String, String)>,
    /// The current track's translation: (track id, language, lines).
    pub(crate) lines: Option<(String, String, Vec<String>)>,
    pub(crate) error: Option<(String, String)>,
    /// The song text last looked at (track id, hash of its lines) and the
    /// lyrics revision it was checked at.
    seen: Option<(String, u64)>,
    seen_rev: u64,
}

/// The language of a text by its common words (None when unclear).
pub(crate) fn detect_language(text: &str) -> Option<&'static str> {
    if text
        .chars()
        .filter(|c| ('\u{400}'..='\u{4ff}').contains(c))
        .count()
        > 10
    {
        return Some("ru");
    }
    const WORDS: &[(&str, &[&str])] = &[
        (
            "en",
            &[
                "the", "you", "and", "i", "to", "it", "me", "my", "is", "that", "in", "your", "we",
                "don't", "i'm", "on", "of", "be",
            ],
        ),
        (
            "de",
            &[
                "ich", "du", "und", "die", "der", "das", "nicht", "ist", "mich", "mir", "ein",
                "es", "wir", "zu", "dich", "sie", "auf",
            ],
        ),
        (
            "es",
            &[
                "que", "de", "el", "la", "y", "en", "no", "me", "te", "mi", "tu", "lo", "por",
                "con", "una", "es", "yo",
            ],
        ),
        (
            "fr",
            &[
                "je", "tu", "le", "la", "et", "de", "les", "pas", "est", "que", "moi", "toi", "un",
                "une", "dans", "c'est", "mon",
            ],
        ),
        (
            "it",
            &[
                "che", "di", "il", "la", "e", "non", "mi", "ti", "sono", "per", "un", "una", "io",
                "tu", "con", "è", "ho",
            ],
        ),
        (
            "nl",
            &[
                "ik", "je", "de", "het", "een", "en", "niet", "is", "van", "dat", "mij", "jij",
                "we", "op", "zijn", "maar", "wat",
            ],
        ),
        (
            "tr",
            &[
                "bir", "ve", "bu", "ben", "sen", "ne", "da", "de", "için", "çok", "gibi", "var",
                "yok", "beni", "seni", "ama", "olan",
            ],
        ),
    ];
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut scores: Vec<(&str, usize)> = WORDS
        .iter()
        .map(|(lang, list)| {
            (
                *lang,
                words.iter().filter(|w| list.contains(&w.as_str())).count(),
            )
        })
        .collect();
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let (best, second) = (scores[0], scores[1]);
    (best.1 >= 3 && best.1 * 2 > second.1 * 3).then_some(best.0)
}

fn storage_key(id: &str, lang: &str) -> String {
    format!("translations/{:016x}-{lang}.json", crate::library::fnv(id))
}

impl Core {
    /// The language song texts are translated into: the app's.
    fn translate_target(&self) -> &'static str {
        if self.settings.language.starts_with("de") {
            "de"
        } else {
            "en"
        }
    }

    fn translate_models(&mut self, host: &dyn Host) -> Option<Arc<Models>> {
        if self.translate.models.is_none() {
            let dir = host
                .storage_path("translation/.keep")
                .and_then(|p| PathBuf::from(p).parent().map(PathBuf::from))?;
            self.translate.models = Some(Arc::new(Models::new(dir)));
        }
        self.translate.models.clone()
    }

    pub(crate) fn translate_available(&self, host: &dyn Host) -> bool {
        host.has_threads() && host.platform() != "web"
    }

    /// `translation` in `lyrics.get`: on, state and progress.
    pub(crate) fn translate_json(&self) -> Value {
        let t = &self.translate;
        let mut v = Value::object().with("on", self.settings.translate);
        if let Some((id, p)) = &t.job {
            if self.queue.current() == Some(id) {
                v = v
                    .with("state", "running")
                    .with("progress", p.load(Ordering::Relaxed) as f64 / 1000.0);
            }
        }
        if let (Some((id, pair)), Some(models)) = (&t.waiting, &t.models) {
            if self.queue.current() == Some(id) {
                v = match models.state(pair) {
                    Some(Download::Running { done, total }) => v
                        .with("state", "downloading")
                        .with("progress", done as f64 / total.max(1) as f64),
                    Some(Download::Checking) => {
                        v.with("state", "downloading").with("progress", 1.0)
                    }
                    Some(Download::Failed(e)) => v.with("state", "failed").with("error", e),
                    None => v.with("state", "downloading").with("progress", 0.0),
                };
            }
        }
        if let Some((id, e)) = &t.error {
            if self.queue.current() == Some(id) {
                v = v.with("state", "failed").with("error", e.as_str());
            }
        }
        v
    }

    /// The current track's translated lines, when they fit its text.
    pub(crate) fn translated_lines(&self, id: &str, n: usize) -> Option<&[String]> {
        let (tid, lang, lines) = self.translate.lines.as_ref()?;
        (self.settings.translate
            && tid == id
            && lang == self.translate_target()
            && lines.len() == n)
            .then_some(lines.as_slice())
    }

    /// `translate.*` commands.
    pub(crate) fn translate_command(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        c: &Value,
    ) -> Value {
        let err = |m: &str| Value::object().with("error", m);
        if !self.translate_available(host) {
            return err("not available here");
        }
        match name {
            "translate.set" => {
                let on = c.bool_or("on", false);
                self.set_setting(host, "translate", Value::from(on));
                self.translate.error = None;
                self.translate.seen = None;
                if on {
                    self.translate_current(host);
                }
                self.revs.lyrics += 1;
                Value::object().with("ok", true)
            }
            "translate.list" => {
                let Some(models) = self.translate_models(host) else {
                    return err("no storage");
                };
                Value::Array(
                    PAIRS
                        .iter()
                        .map(|p| {
                            let id = p.id();
                            let mut v = Value::object()
                                .with("id", id.as_str())
                                .with("from", p.from)
                                .with("to", p.to)
                                .with("size", p.size())
                                .with(
                                    "downloaded",
                                    p.files
                                        .iter()
                                        .all(|f| models.dir.join(&id).join(f.0).is_file()),
                                );
                            if let Some(Download::Running { done, total }) = models.state(&id) {
                                v = v
                                    .with("state", "downloading")
                                    .with("done", done)
                                    .with("total", total);
                            }
                            v
                        })
                        .collect(),
                )
            }
            "translate.delete" => {
                let Some(models) = self.translate_models(host) else {
                    return err("no storage");
                };
                let id = c.str_or("id", "").to_string();
                if !PAIRS.iter().any(|p| p.id() == id) {
                    return err("no such model");
                }
                if self
                    .translate
                    .loaded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .is_some_and(|(l, _)| *l == id)
                {
                    *self
                        .translate
                        .loaded
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = None;
                }
                models.delete_files(&id);
                Value::object().with("ok", true)
            }
            _ => err("unknown command"),
        }
    }

    /// Translates the current track's song text (from the device's copy,
    /// else with the model, loading it first when needed).
    pub(crate) fn translate_current(&mut self, host: &mut dyn Host) {
        if !self.settings.translate || !self.translate_available(host) {
            return;
        }
        let Some(id) = self.queue.current().cloned() else {
            return;
        };
        if self.lyrics.id != id
            || self.translate.job.as_ref().is_some_and(|j| j.0 == id)
            || self.translate.waiting.as_ref().is_some_and(|w| w.0 == id)
        {
            return;
        }
        let Some((lyrics, _)) = self.lyrics.found.as_ref() else {
            return;
        };
        let to = self.translate_target();
        let texts: Vec<String> = lyrics.lines.iter().map(|l| l.text.clone()).collect();
        if self.translated_lines(&id, texts.len()).is_some() {
            return;
        }
        // Kept from before.
        if let Some(saved) = host
            .storage_read(&storage_key(&id, to))
            .and_then(|b| bl_json::parse_bytes(&b).ok())
            .and_then(|v| v.as_array().cloned())
        {
            let lines: Vec<String> = saved
                .iter()
                .map(|v| v.as_str().unwrap_or("").to_string())
                .collect();
            if lines.len() == texts.len() {
                self.translate.lines = Some((id, to.to_string(), lines));
                self.revs.lyrics += 1;
                return;
            }
        }
        let all = texts.join("\n");
        let Some(from) = detect_language(&all) else {
            self.translate.error = Some((
                id,
                self.tr("The song text's language was not recognised")
                    .to_string(),
            ));
            self.revs.lyrics += 1;
            return;
        };
        if from == to {
            // Already in the app's language: nothing to show.
            self.translate.lines = None;
            return;
        }
        let Some(pair) = PAIRS.iter().find(|p| p.from == from && p.to == to) else {
            self.translate.error = Some((
                id,
                self.tr("No translation model for this language")
                    .to_string(),
            ));
            self.revs.lyrics += 1;
            return;
        };
        let Some(models) = self.translate_models(host) else {
            return;
        };
        let pair_id = pair.id();
        let folder = models.dir.join(&pair_id);
        if !pair.files.iter().all(|f| folder.join(f.0).is_file()) {
            let ready = self.translate.ready.clone();
            let pid = pair_id.clone();
            models.download_files(&pair_id, pair.specs(), move |_| ready.post(pid));
            self.translate.waiting = Some((id, pair_id));
            self.revs.lyrics += 1;
            return;
        }
        let progress = Arc::new(AtomicU32::new(0));
        self.translate.job = Some((id.clone(), progress.clone()));
        self.translate.error = None;
        let (loaded, results, to) = (
            self.translate.loaded.clone(),
            self.translate.results.clone(),
            to.to_string(),
        );
        host.spawn(Box::new(move || {
            let res = translate_lines(&loaded, &pair_id, &folder, &texts, &progress);
            results.post((id, to, res));
        }));
        self.revs.lyrics += 1;
    }

    pub(crate) fn poll_translate(&mut self, host: &mut dyn Host) {
        let mut again = self.lyrics_changed();
        for pair in self.translate.ready.take() {
            if self.translate.waiting.as_ref().is_some_and(|w| w.1 == pair) {
                self.translate.waiting = None;
                again = true;
            }
        }
        if let Some((_, pair)) = &self.translate.waiting {
            if let Some(Download::Failed(e)) =
                self.translate.models.as_ref().and_then(|m| m.state(pair))
            {
                let (id, _) = self.translate.waiting.take().expect("waiting");
                self.translate.error = Some((id, e));
                self.revs.lyrics += 1;
            }
        }
        for (id, lang, res) in self.translate.results.take() {
            if self.translate.job.as_ref().is_some_and(|j| j.0 == id) {
                self.translate.job = None;
            }
            match res {
                Ok(lines) => {
                    let v = Value::Array(lines.iter().map(|l| Value::from(l.as_str())).collect());
                    host.storage_write(&storage_key(&id, &lang), v.to_json().as_bytes());
                    self.translate.lines = Some((id, lang, lines));
                }
                Err(e) => self.translate.error = Some((id, e)),
            }
            self.revs.lyrics += 1;
            again = true;
        }
        if again {
            self.translate_current(host);
            self.translate.seen_rev = self.revs.lyrics;
        }
    }

    /// Whether the current song text changed since it was last looked at
    /// (a new track, a loaded, imported or timed text).
    fn lyrics_changed(&mut self) -> bool {
        if !self.settings.translate || self.translate.seen_rev == self.revs.lyrics {
            return false;
        }
        self.translate.seen_rev = self.revs.lyrics;
        let Some((lyrics, _)) = self.lyrics.found.as_ref() else {
            return false;
        };
        let mut h = 0xcbf29ce484222325u64;
        for l in &lyrics.lines {
            for b in l.text.bytes().chain([b'\n']) {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
        }
        let key = (self.lyrics.id.clone(), h);
        if self.translate.seen.as_ref() == Some(&key) {
            return false;
        }
        self.translate.seen = Some(key);
        true
    }
}

/// Loads the model (kept for the next song) and translates (worker thread).
fn translate_lines(
    loaded: &Loaded,
    pair: &str,
    folder: &std::path::Path,
    lines: &[String],
    progress: &AtomicU32,
) -> Result<Vec<String>, String> {
    let model = {
        let mut l = loaded.lock().unwrap_or_else(|e| e.into_inner());
        match l.as_ref().filter(|(p, _)| p == pair) {
            Some((_, m)) => m.clone(),
            None => {
                *l = None;
                let m = Arc::new(bl_mt::Translator::load(folder)?);
                *l = Some((pair.to_string(), m.clone()));
                m
            }
        }
    };
    // Each line once (choruses repeat).
    let mut done: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        let text = if !t.chars().any(char::is_alphanumeric) {
            String::new()
        } else if let Some(d) = done.get(t) {
            d.clone()
        } else {
            let r = model.translate(&[t]).pop().unwrap_or_default();
            done.insert(t, r.clone());
            r
        };
        out.push(text);
        progress.store(
            ((i + 1) * 1000 / lines.len().max(1)) as u32,
            Ordering::Relaxed,
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_are_recognised() {
        assert_eq!(
            detect_language(
                "You say you love me, I say you crazy, we're nothing more than friends"
            ),
            Some("en")
        );
        assert_eq!(
            detect_language("Ich weiß nicht, was soll es bedeuten, dass ich so traurig bin"),
            Some("de")
        );
        assert_eq!(
            detect_language(
                "No sé qué hacer, porque yo te quiero y tú no me dejas en paz con tu amor"
            ),
            Some("es")
        );
        assert_eq!(detect_language("la la la"), None);
        // Every pair's files are complete.
        for p in PAIRS {
            assert_eq!(p.files.len(), 5);
            assert!(p.files.iter().all(|f| f.2.len() == 64));
        }
    }
}
