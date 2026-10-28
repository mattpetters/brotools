# BroTools app icon

**A "b" over a wave**, mirror-symmetric: a white ring centred on a teal gradient tile, a
symmetric wave (crest, trough, crest) crossing the lower third and merging into the ring, and a
geometric lowercase "b" (round-capped stem, circular bowl) above it.

## Palette

| Colour | Hex | Used for |
|---|---|---|
| Teal, top | `#2fc6dd` | Gradient start |
| Teal, bottom | `#0b8aa3` | Gradient end |
| White | `#ffffff` | Ring, wave, letter |

## Geometry

A 512 × 512 viewBox, clipped to a rounded square with `rx=112`. Keep that clip: macOS 26+ applies its
own squircle mask, and only an icon whose opaque body covers that mask escapes the grey fallback
frame ("icon jail"). A true superellipse leaves transparent corners and gets framed (checked on
macOS 27.2 with `NSWorkspace.icon(forFile:)`). Windows and Linux use the full
tile. The macOS icons (`soundcraft.icns`, `soundcraft-macos-512.png`) sit on Apple's grid: an
824/1024 body with a transparent margin.

## Provenance and licence

Drawn from scratch as hand-placed SVG paths for brotools; no source image was traced or copied. MIT OR Apache-2.0, like the code (see `ATTRIBUTION.md`).

## Files

| File | What it is |
|---|---|
| `icon.svg` | The canonical vector |
| `soundcraft-1024.png` | 1024 px render |
| `soundcraft-macos-512.png` | Runtime window/Dock icon on macOS (Apple grid margin) |
| `soundcraft.icns` | macOS bundle icon |
| `soundcraft.ico` | Windows icon (16–256 px), embedded in `soundcraft.exe` and used by the MSI |
| `hicolor/<size>x<size>/apps/ai.storyteller.soundcraft.png` | Linux/FreeBSD icon theme, 16–512 px, plus `scalable/` |

Regenerate everything with `packaging/icons.sh` (needs `resvg`, and `iconutil` on macOS for the
`.icns`).
