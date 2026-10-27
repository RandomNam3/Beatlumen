//! Server-side whole-track analysis (`GET /api/tracks/{id}/analysis`).
//!
//! The track is decoded completely with `oc_media`, downmixed to mono and
//! analysed with `oc_analysis::analyze`. Results are cached as
//! `data/cache/analysis/<id>_<cache key hex>.ocan` (the `oc_analysis::encode`
//! blob). At most two analyses run at a time; concurrent requests for the
//! same track and configuration wait for the running computation instead of
//! starting a duplicate.

use crate::library::Entry;
use crate::util::{self, Semaphore};
use oc_analysis::AnalysisConfig;
use oc_json::Value;
use std::collections::HashMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Upper bound for chroma frames: the analysis builds three n×n
/// self-similarity matrices, so very long tracks get a coarser transition
/// resolution instead of gigabytes of memory.
const MAX_CHROMA_FRAMES: usize = 4000;

/// Concurrent analyses.
const MAX_CONCURRENT: usize = 2;

pub type Failure = (u16, String);
type Outcome = Result<Arc<Vec<u8>>, Failure>;

struct Slot {
    result: Mutex<Option<Outcome>>,
    cv: Condvar,
}

pub struct Analyzer {
    dir: PathBuf,
    max_minutes: u64,
    inflight: Mutex<HashMap<String, Arc<Slot>>>,
    sem: Semaphore,
}

/// Removes the in-flight entry and wakes waiters even if the owner unwinds.
struct OwnerGuard<'a> {
    analyzer: &'a Analyzer,
    key: String,
    slot: Arc<Slot>,
}

impl Drop for OwnerGuard<'_> {
    fn drop(&mut self) {
        {
            let mut r = self.slot.result.lock().unwrap_or_else(|e| e.into_inner());
            if r.is_none() {
                *r = Some(Err((500, "analysis failed".into())));
            }
        }
        self.slot.cv.notify_all();
        self.analyzer.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.key);
    }
}

/// Clamps a configuration to sane bounds (requests are untrusted; huge
/// filter radii or FFT sizes would be a denial of service).
pub fn sanitize(mut c: AnalysisConfig) -> AnalysisConfig {
    let f = |v: f32, lo: f32, hi: f32, d: f32| if v.is_finite() { v.clamp(lo, hi) } else { d };
    c.fft_size = c.fft_size.clamp(256, 16384);
    c.spectral_smoothing = c.spectral_smoothing.clamp(1, 50);
    c.spectral_separation = f(c.spectral_separation, 0.01, 100.0, 3.0);
    c.chroma_smoothing = c.chroma_smoothing.clamp(1, 200);
    c.chroma_down_sampling = c.chroma_down_sampling.clamp(1, 200);
    c.major_transition_window = c.major_transition_window.clamp(1, 200);
    c.minor_transition_window = c.minor_transition_window.clamp(1, 200);
    c.novelty.truncate(8);
    for n in &mut c.novelty {
        n.normalization = f(n.normalization, 0.001, 1000.0, 1.0);
        n.start_freq = f(n.start_freq, 0.0, 100_000.0, 0.0);
        n.end_freq = f(n.end_freq, 0.0, 100_000.0, 300.0);
    }
    c
}

/// Builds the analysis configuration from the `cfg` query parameter
/// (`{"analysisConfig":{..},"noveltyConfigs":{..}}`); defaults when absent.
pub fn config_from_query(cfg: Option<&str>) -> Result<AnalysisConfig, String> {
    let Some(text) = cfg.filter(|s| !s.trim().is_empty()) else {
        return Ok(sanitize(AnalysisConfig::default()));
    };
    let v = oc_json::parse(text).map_err(|e| format!("invalid cfg JSON: {e}"))?;
    if v.as_object().is_none() {
        return Err("cfg must be a JSON object".into());
    }
    let only = Value::object().with("analysisConfig", v.get("analysisConfig").clone()).with("noveltyConfigs", v.get("noveltyConfigs").clone());
    let preset = oc_fx::Preset::from_json(&only, "cfg", &mut oc_fx::Rng::new(0))?;
    Ok(sanitize(preset.analysis_config()))
}

impl Analyzer {
    pub fn new(dir: PathBuf, max_minutes: u64) -> Analyzer {
        Analyzer { dir, max_minutes, inflight: Mutex::new(HashMap::new()), sem: Semaphore::new(MAX_CONCURRENT) }
    }

    pub fn cache_path(&self, id: &str, cfg: &AnalysisConfig) -> PathBuf {
        self.dir.join(format!("{id}_{:016x}.ocan", cfg.cache_key()))
    }

    /// Reads a cached blob if it is newer than the track and valid.
    fn cached(path: &Path, e: &Entry) -> Option<Vec<u8>> {
        let meta = std::fs::metadata(path).ok()?;
        if util::mtime_ns(&meta) < e.mtime_ns {
            return None;
        }
        let data = std::fs::read(path).ok()?;
        oc_analysis::decode(&data).is_some().then_some(data)
    }

    /// Returns the analysis blob for a track, computing it if needed.
    pub fn get(&self, e: &Entry, cfg: &AnalysisConfig) -> Result<Vec<u8>, Failure> {
        let path = self.cache_path(&e.track.id, cfg);
        if let Some(b) = Self::cached(&path, e) {
            return Ok(b);
        }
        let key = path.to_string_lossy().into_owned();
        let (slot, owner) = {
            let mut m = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            match m.get(&key) {
                Some(s) => (s.clone(), false),
                None => {
                    let s = Arc::new(Slot { result: Mutex::new(None), cv: Condvar::new() });
                    m.insert(key.clone(), s.clone());
                    (s, true)
                }
            }
        };
        if !owner {
            let mut r = slot.result.lock().unwrap_or_else(|e| e.into_inner());
            while r.is_none() {
                r = slot.cv.wait(r).unwrap_or_else(|e| e.into_inner());
            }
            return r.clone().unwrap_or_else(|| Err((500, "analysis failed".into()))).map(|a| (*a).clone());
        }
        let guard = OwnerGuard { analyzer: self, key, slot };
        let outcome: Outcome = (|| {
            // Another request may have finished between the check and now.
            if let Some(b) = Self::cached(&path, e) {
                return Ok(Arc::new(b));
            }
            let _permit = self.sem.acquire();
            let t0 = Instant::now();
            let blob = self.compute(e, cfg)?;
            crate::info!("analysed {} in {:.1} s", e.rel, t0.elapsed().as_secs_f32());
            if let Err(err) = util::write_atomic(&path, &blob) {
                crate::warn!("cannot write analysis cache {}: {err}", path.display());
            }
            Ok(Arc::new(blob))
        })();
        *guard.slot.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome.clone());
        drop(guard);
        outcome.map(|a| (*a).clone())
    }

    fn compute(&self, e: &Entry, cfg: &AnalysisConfig) -> Result<Vec<u8>, Failure> {
        let file = std::fs::File::open(&e.abs).map_err(|_| (404, "track file not found".to_string()))?;
        let ext = e.ext();
        let max_minutes = self.max_minutes;
        let rel = e.rel.clone();
        util::catch("analysis", move || -> Result<Vec<u8>, Failure> {
            let src: Box<dyn oc_media::Source> = Box::new(BufReader::with_capacity(256 * 1024, file));
            let mut dec = oc_media::open(src, Some(&ext)).map_err(|err| (415, format!("cannot decode track: {err}")))?;
            let spec = dec.spec();
            if spec.sample_rate == 0 || spec.channels == 0 {
                return Err((415, "cannot decode track: invalid stream parameters".into()));
            }
            let sr = spec.sample_rate as u64;
            let limit = if max_minutes == 0 { usize::MAX } else { max_minutes.saturating_mul(60).saturating_mul(sr).min(usize::MAX as u64) as usize };
            // Never trust header lengths for allocations beyond a sane size.
            let cap = dec.total_frames().unwrap_or(0).min(limit as u64).min(20 * 60 * sr) as usize;
            let mut mono: Vec<f32> = Vec::with_capacity(cap);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match dec.next_chunk(&mut buf) {
                    Ok(more) => {
                        oc_analysis::to_mono(&buf, spec.channels as usize, &mut mono);
                        if mono.len() > limit {
                            return Err((413, format!("track is longer than the server's analysis limit ({max_minutes} minutes)")));
                        }
                        if !more {
                            break;
                        }
                    }
                    Err(err) if mono.len() >= sr as usize => {
                        crate::warn!("decode error in {rel} after {:.1} s: {err}; analysing the decoded part", mono.len() as f64 / sr as f64);
                        break;
                    }
                    Err(err) => return Err((422, format!("cannot decode track: {err}"))),
                }
            }
            if mono.is_empty() {
                return Err((422, "track contains no audio".into()));
            }
            let mut cfg = cfg.clone();
            let hop = (cfg.fft_size.max(64).next_power_of_two() / 2).max(1);
            let min_down = mono.len().div_ceil(hop).div_ceil(MAX_CHROMA_FRAMES).max(1);
            cfg.chroma_down_sampling = cfg.chroma_down_sampling.max(min_down);
            let a = oc_analysis::analyze(&mono, spec.sample_rate, &cfg, &mut |_| true).ok_or((500, "analysis failed".to_string()))?;
            Ok(oc_analysis::encode(&a))
        })
        .map_err(|m| (500, m))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_config() {
        let d = config_from_query(None).unwrap();
        assert_eq!(d, AnalysisConfig::default());
        let c = config_from_query(Some(r#"{"analysisConfig":{"chromaSmoothing":12},"noveltyConfigs":{"Low":{"spectrum":"Raw","normalization":4,"startFreq":0,"endFreq":200}}}"#)).unwrap();
        assert_eq!(c.chroma_smoothing, 12);
        assert_eq!(c.novelty[0].end_freq, 200.0);
        assert_ne!(c.cache_key(), d.cache_key());
        let huge = config_from_query(Some(r#"{"analysisConfig":{"spectralSmoothing":1e12,"chromaDownSampling":-5}}"#)).unwrap();
        assert_eq!((huge.spectral_smoothing, huge.chroma_down_sampling), (50, 1));
        assert!(config_from_query(Some("[1,2]")).is_err());
        assert!(config_from_query(Some("{bad")).is_err());
    }
}
