"""Test songs for the speech model comparison, with reference times from
LRCLIB (https://lrclib.net): for each audio file its tags (title, artist,
album, length) are looked up, the synced song text is saved as the
reference and the audio converted to 16 kHz mono. Only these four values
are sent. One song at a time, a pause between songs, and as long a wait as
the service asks for when it is busy.

Usage: lrclib_refs.py OUT_DIR AUDIO_FILE...
Then:  eval.py MODELS_DIR OUT_DIR/*/

A reference can belong to another version of the song (same length,
other tempo or edit): compare it with a recognition (cmp_text.py) before
trusting it."""
import json, os, random, re, subprocess, sys, time, urllib.parse, urllib.request, urllib.error

UA = "Beatlumen-model-eval (https://github.com/RandomNam3/Beatlumen)"
BASE = "https://lrclib.net/api/get"


def tags(path):
    out = subprocess.run(
        ["ffprobe", "-v", "quiet", "-show_entries", "format=duration:format_tags=title,artist,album",
         "-of", "json", path], capture_output=True, text=True).stdout
    f = json.loads(out or "{}").get("format", {})
    t = {k.lower(): v for k, v in f.get("tags", {}).items()}
    return t.get("title", ""), t.get("artist", ""), t.get("album", ""), float(f.get("duration", 0) or 0)


def get(params):
    url = BASE + "?" + urllib.parse.urlencode(params)
    for _ in range(8):
        req = urllib.request.Request(url, headers={"User-Agent": UA})
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            if e.code in (429, 503):
                wait = float(e.headers.get("Retry-After") or 5)
                time.sleep(min(wait, 600) * (1 + random.random() * 0.3))
                continue
            raise
    raise RuntimeError("lrclib.net stays busy")


def lookup(title, artist, album, duration):
    base = {"track_name": title, "artist_name": artist}
    if duration >= 1:
        base["duration"] = round(duration)
    tries = [dict(base, album_name=album)] if album else []
    # Artists as "A, B, C": the first one alone; titles with " - X Remix"
    # as they are (LRCLIB names versions in the title too).
    tries.append(base)
    first = re.split(r",|&| feat\.? | ft\.? ", artist)[0].strip()
    if first and first != artist:
        tries.append(dict(base, artist_name=first))
    for p in tries:
        rec = get(p)
        time.sleep(0.5)
        if rec and rec.get("syncedLyrics") and abs(float(rec.get("duration", 0)) - duration) <= 3:
            return rec
    return None


def main():
    out, files = sys.argv[1], sys.argv[2:]
    os.makedirs(out, exist_ok=True)
    for path in files:
        title, artist, album, duration = tags(path)
        name = re.sub(r"[^A-Za-z0-9]+", "_", os.path.splitext(os.path.basename(path))[0]).strip("_")[:60]
        d = os.path.join(out, name)
        if os.path.exists(os.path.join(d, name + ".lrc")):
            print(name, "done before")
            continue
        if not title or not artist:
            print(name, "no title/artist tags, skipped")
            continue
        rec = lookup(title, artist, album, duration)
        if not rec:
            print(name, "no synced text on LRCLIB")
            continue
        os.makedirs(d, exist_ok=True)
        subprocess.run(["ffmpeg", "-v", "quiet", "-y", "-i", path, "-ac", "1", "-ar", "16000",
                        "-c:a", "pcm_s16le", os.path.join(d, name + ".wav")], check=True)
        open(os.path.join(d, name + ".lrc"), "w", encoding="utf-8").write(rec["syncedLyrics"])
        json.dump({"lrclib_id": rec.get("id"), "title": title, "artist": artist, "album": album,
                   "duration": duration, "lrclib_duration": rec.get("duration")},
                  open(os.path.join(d, "meta.json"), "w"), indent=1)
        print(name, "reference", rec.get("id"))


main()
