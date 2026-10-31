"""Speech model comparison: for each model and song, (a) the song written
down without a text (word error rate against the LRC's words), (b) the
plain text timed (line start error against the LRC's times), and the run
times. Usage: eval.py MODELS_DIR SONG_DIR... (each SONG_DIR has x.wav and
x.lrc).

Environment:
  TRANSCRIBE  the transcribe example binary (default:
              target/release/examples/transcribe in the repository)
  MODELS      the models to run (default: base-q5_1 small large-v3-turbo-q5_0)
  OUT         where outputs go (default: speech-eval-out in the current folder)"""
import json, os, re, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
BIN = os.environ.get("TRANSCRIBE", os.path.join(ROOT, "target", "release", "examples", "transcribe"))
MODELS = os.environ.get("MODELS", "base-q5_1 small large-v3-turbo-q5_0").split()
OUT = os.environ.get("OUT", "speech-eval-out")

TIME = re.compile(r"\[(\d+):(\d+(?:\.\d+)?)\]")
WORD_TAG = re.compile(r"<\d+:\d+(?:\.\d+)?>")


def lrc_lines(text):
    out = []
    for raw in text.splitlines():
        times = TIME.findall(raw)
        rest = TIME.sub("", raw)
        rest = WORD_TAG.sub("", rest).strip()
        if raw.strip().startswith("[") and not times:
            continue
        for m, s in times:
            out.append((int(m) * 60 + float(s), rest))
    return sorted(out)


def words(text):
    text = text.lower().replace("'", "")
    return re.findall(r"[a-z0-9]+", text)


def wer(ref, hyp):
    n, m = len(ref), len(hyp)
    prev = list(range(m + 1))
    for i in range(1, n + 1):
        cur = [i] + [0] * m
        for j in range(1, m + 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ref[i - 1] != hyp[j - 1]))
        prev = cur
    return prev[m] / max(1, n)


def run(model, wav, text_file=None):
    args = [BIN, model, wav, "--lrc"] + ([text_file] if text_file else [])
    t0 = time.time()
    p = subprocess.run(args, capture_output=True, text=True)
    return p.stdout, time.time() - t0


def main():
    models_dir, songs = sys.argv[1], sys.argv[2:]
    os.makedirs(OUT, exist_ok=True)
    results = []
    for song in songs:
        name = os.path.basename(song.rstrip("/"))
        ref = lrc_lines(open(os.path.join(song, name + ".lrc"), encoding="utf-8").read())
        ref_text = [t for _, t in ref if re.search(r"\w", t)]
        plain = os.path.join(OUT, name + ".txt")
        open(plain, "w").write("\n".join(ref_text) + "\n")
        ref_words = words(" ".join(ref_text))
        wav = os.path.join(song, name + ".wav")
        for model in MODELS:
            path = os.path.join(models_dir, "ggml-%s.bin" % model)
            if not os.path.exists(path):
                continue
            # (a) written down without a text.
            out_a, secs_a = run(path, wav)
            open(os.path.join(OUT, "%s-%s-none.lrc" % (name, model)), "w").write(out_a)
            hyp = words(" ".join(t for _, t in lrc_lines(out_a)))
            w = wer(ref_words, hyp) if hyp else 1.0
            # (b) the plain text timed: line starts against the LRC's.
            out_b, secs_b = run(path, wav, plain)
            open(os.path.join(OUT, "%s-%s-text.lrc" % (name, model)), "w").write(out_b)
            got = [(t, x) for t, x in lrc_lines(out_b) if re.search(r"\w", x)]
            want = [(t, x) for t, x in ref if re.search(r"\w", x)]
            errs = []
            if len(got) == len(want):
                errs = [abs(a[0] - b[0]) for a, b in zip(got, want)]
            errs.sort()
            r = {
                "song": name,
                "model": model,
                "wer": round(w, 3),
                "secs_none": round(secs_a, 1),
                "lines": "%d/%d" % (len(got), len(want)),
                "line_err_median": round(errs[len(errs) // 2], 2) if errs else None,
                "within_0_5s": round(sum(e <= 0.5 for e in errs) / len(errs), 3) if errs else None,
                "within_1s": round(sum(e <= 1.0 for e in errs) / len(errs), 3) if errs else None,
                "secs_text": round(secs_b, 1),
                "text_error": out_b.strip()[:120] if out_b.startswith("error") else None,
            }
            print(json.dumps(r), flush=True)
            results.append(r)
    json.dump(results, open(os.path.join(OUT, "results.json"), "w"), indent=1)


main()
