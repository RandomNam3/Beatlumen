"""One LRCLIB lookup: lrclib_get.py TITLE ARTIST DURATION OUT.lrc (prints the matches)."""
import json, random, sys, time, urllib.parse, urllib.request, urllib.error
UA = "Beatlumen-model-eval (https://github.com/RandomNam3/Beatlumen)"
title, artist, dur, out = sys.argv[1], sys.argv[2], float(sys.argv[3]), sys.argv[4]
url = "https://lrclib.net/api/search?" + urllib.parse.urlencode({"track_name": title, "artist_name": artist})
for _ in range(10):
    try:
        with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=20) as r:
            recs = json.loads(r.read()); break
    except urllib.error.HTTPError as e:
        if e.code in (429, 503):
            time.sleep(float(e.headers.get("Retry-After") or 5) * (1 + random.random() * .3)); continue
        raise
recs = [r for r in recs if r.get("syncedLyrics") and abs(float(r.get("duration") or 0) - dur) <= 3]
for r in recs: print(r["id"], r["trackName"], "|", r["artistName"], "|", r["albumName"], r["duration"])
if recs: open(out, "w", encoding="utf-8").write(recs[0]["syncedLyrics"])
