"""Compares the line times of two LRC files line by line, pairing lines by
their text (not their position): the median and spread of the time
differences of paired lines. Usage: cmp_text.py A.lrc B.lrc [v]"""
import difflib, re, sys
T = re.compile(r"\[(\d+):(\d+(?:\.\d+)?)\]"); W = re.compile(r"<\d+:\d+(?:\.\d+)?>")
def L(p):
    out = []
    for raw in open(p, encoding="utf-8"):
        ts = T.findall(raw); rest = W.sub("", T.sub("", raw)).strip()
        for m, s in ts:
            if re.search(r"\w", rest): out.append((int(m) * 60 + float(s), rest))
    return sorted(out)
norm = lambda s: re.sub(r"[^a-z0-9 ]", "", s.lower().replace("'", "").replace("in ", "ing "))
a, b = L(sys.argv[1]), L(sys.argv[2])
sm = difflib.SequenceMatcher(None, [norm(x) for _, x in a], [norm(x) for _, x in b], autojunk=False)
pairs = [(a[i], b[j]) for blk in sm.get_matching_blocks() for i, j in zip(range(blk.a, blk.a + blk.size), range(blk.b, blk.b + blk.size))]
d = sorted(y[0] - x[0] for x, y in pairs)
ad = sorted(abs(v) for v in d)
print("%d/%d lines paired, offset median %+.2f, |diff| median %.2f, within 0.5 s %.2f, within 1 s %.2f" % (
    len(pairs), len(a), d[len(d) // 2], ad[len(ad) // 2], sum(v <= .5 for v in ad) / len(ad), sum(v <= 1 for v in ad) / len(ad)))
if len(sys.argv) > 3:
    for x, y in pairs: print("%+6.2f %7.2f %s" % (y[0] - x[0], x[0], x[1][:50]))
