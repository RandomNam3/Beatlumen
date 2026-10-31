"""Median offset (anchor - LRC line time) of matched lines, by halves.
offset.py REF.lrc ANCHORS.txt"""
import re, sys
T = re.compile(r"\[(\d+):(\d+(?:\.\d+)?)\]")
ref = []
for raw in open(sys.argv[1], encoding="utf-8"):
    ts = T.findall(raw); rest = re.sub(r"<[^>]*>", "", T.sub("", raw)).strip()
    if ts and re.search(r"\w", rest): ref.append((int(ts[0][0]) * 60 + float(ts[0][1]), rest))
anc = []
for l in open(sys.argv[2], encoding="utf-8"):
    if l.startswith("#"): continue
    anc.append((float(l[:7]), l[8] == "*"))
pairs = [(r[0], a[0] - r[0]) for r, a in zip(ref, anc) if a[1]]
def med(v): v = sorted(v); return v[len(v) // 2] if v else float("nan")
h = len(pairs) // 2
print(len(ref), len(anc), "matched", len(pairs), "median %.2f first half %.2f second half %.2f" % (
    med([d for _, d in pairs]), med([d for _, d in pairs[:h]]), med([d for _, d in pairs[h:]])))
