# M14 qmldiff tooling & xochitl QML extraction

How the M14 capture hook (`app/xovi/retaskableSend.qmd`) was developed and verified
*offline*, so changes don't require a blind deploy-and-pray loop on hardware.

## qmldiff CLI (the offline workhorse)

`qmldiff` (github.com/asivery/qmldiff, Rust — `cargo build`) reads xochitl's `.qmd`
patch language and applies it to QML source. Two operations we rely on:

- **Unhash / hash a `.qmd`:** `qmldiff hash-diffs <hashtab> <file.qmd> [-r]`
  (`-r` = reverse/unhash, in place). The device's hashtab lives at
  `/home/root/xovi/exthome/qt-resource-rebuilder/hashtab` (qmldiff's own binary
  format; the CLI reads it directly). Unhashing turns the obfuscated `~&123&~`
  tokens in the stock extensions into readable xochitl symbol names so you can
  learn from them.
- **Apply diffs offline:** `qmldiff apply-diffs [--hashtab <ht>] <root> <out> <diffs...> -f`
  where `<root>` mirrors the resource path (e.g.
  `root/qml/device/view/documentview/HwcSelection.qml`). **Readable diffs apply
  without a hashtab** — qmldiff matches real identifiers directly, which is why our
  `.qmd` ships readable. Always apply offline and eyeball the emitted QML before
  deploying.

## Extracting xochitl's QML (to apply against the real source)

xochitl's QML is compiled into the binary's Qt6 resources as **zstd frames**.
`extract_qml_zstd.py` brute-scans `/usr/bin/xochitl` for zstd frames, inflates
each (Python 3.14 stdlib `compression.zstd`), and keeps the QML-looking ones:

    python3 extract_qml_zstd.py /path/to/xochitl <outdir>

then grep the output for the type/id you need (e.g. `signal menuRendered`,
`id: selectionMenu`). This is a local, learn-the-internals step only — **do not
redistribute the extracted xochitl QML** (it's reMarkable's IP). A cleaner
alternative on older qt-resource-rebuilder builds is the `QMLDIFF_EXTRACT_TREE`
env var, but it was removed upstream (2025-03) and isn't in the 2026 device `.so`.

## The M14 capture hook, in three patches

The handwriting lasso menu is `SceneSelectionHandler.qml`'s `?#tools` (NOT the
glyph menu). Recognized text for handwriting requires a convert step, so the hook
mirrors the stock convert-to-text wiring across three files — see the header of
`app/xovi/retaskableSend.qmd` for the A/B/C breakdown.
