#!/usr/bin/env python3
"""Brute-scan an ELF for zstd frames, decompress each, keep QML-looking text."""
import sys, os, re
from compression import zstd

data = open(sys.argv[1], "rb").read()
outdir = sys.argv[2]
os.makedirs(outdir, exist_ok=True)

MAGIC = b"\x28\xb5\x2f\xfd"
offsets = []
start = 0
while True:
    j = data.find(MAGIC, start)
    if j < 0:
        break
    offsets.append(j)
    start = j + 1

print(f"{len(offsets)} zstd frames")
kept = []
for off in offsets:
    d = zstd.ZstdDecompressor()
    try:
        out = d.decompress(data[off:off + 8_000_000])
    except Exception:
        continue
    if not out or len(out) < 40:
        continue
    try:
        text = out.decode("utf-8")
    except UnicodeDecodeError:
        continue
    if "import " in text or "selectionMenu" in text or "ContextualMenu" in text or "iconSource" in text:
        kept.append((off, text))

print(f"{len(kept)} QML-ish frames")
for idx, (off, text) in enumerate(kept):
    # name by first import-free Type declaration or qml comment
    fn = f"{outdir}/q_{idx:03d}_{off:#x}.qml"
    open(fn, "w").write(text)

for idx, (off, text) in enumerate(kept):
    if "selectionMenu" in text or "GlyphSelection" in text:
        print(f"  >> q_{idx:03d}_{off:#x}.qml has selectionMenu/GlyphSelection ({len(text)}b)")
