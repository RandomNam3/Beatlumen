//! Optional decode speed measurement: set OC_MEDIA_BENCH=<file> and run
//! with `--nocapture`.

#[test]
fn bench_env_file() {
    let Ok(list) = std::env::var("OC_MEDIA_BENCH") else { return };
    for path in list.split(',') {
        let f = std::fs::File::open(path).unwrap();
        let ext = path.rsplit('.').next().map(|s| s.to_string());
        let t0 = std::time::Instant::now();
        let mut d = bl_media::open(Box::new(f), ext.as_deref()).unwrap();
        let mut out = Vec::new();
        let mut frames = 0usize;
        while d.next_chunk(&mut out).unwrap() {
            frames += out.len() / d.spec().channels as usize;
            out.clear();
        }
        let secs = t0.elapsed().as_secs_f64();
        let audio = frames as f64 / d.spec().sample_rate as f64;
        println!("{path}: {} {:.1}s audio in {:.3}s = {:.0}x realtime", d.codec_name(), audio, secs, audio / secs);
    }
}

/// Resampler throughput (set OC_MEDIA_BENCH_RESAMPLE=1).
#[test]
fn bench_resampler() {
    if std::env::var("OC_MEDIA_BENCH_RESAMPLE").is_err() {
        return;
    }
    for (a, b) in [(44100u32, 48000u32), (48000, 44100), (44100, 44101), (96000, 48000)] {
        let secs = 30usize;
        let input: Vec<f32> = (0..a as usize * secs * 2).map(|i| ((i as f32) * 0.001).sin() * 0.5).collect();
        let mut r = bl_media::Resampler::new(a, b, 2);
        let mut out = Vec::new();
        let t0 = std::time::Instant::now();
        for c in input.chunks(4096) {
            r.process(c, &mut out);
        }
        let el = t0.elapsed().as_secs_f64();
        println!("resample {a}->{b} stereo: {:.0}x realtime (latency {} frames)", secs as f64 / el, r.latency());
    }
}
