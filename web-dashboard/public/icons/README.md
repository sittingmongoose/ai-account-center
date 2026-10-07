# PWA icons (DESIGN-MOBILE.md 6.4)

Derived from the existing Apex artwork (`appicon-light.svg` /
`appicon-light-1024.png`: squircle body, rim, soft shadow, transparent corners).

- `aac-192.png`, `aac-512.png` (`purpose: any`): the 1024 px export scaled with
  `ffmpeg -vf scale=W:H:flags=lanczos -sws_dither ed`. Transparent corners kept.
- `aac-maskable-192.png`, `aac-maskable-512.png` (`purpose: maskable`): full-bleed
  square filled with the icon body gradient (`#FEFFFF` to `#F4F7FA` at 45% to
  `#DFE6EE`, top to bottom). The Apex glyph (dark legs plus blue bar, same
  gradients, taken as the `<g>` paths from `appicon-light.svg`) is centred with
  its height at 52% of the canvas, nudged down 1% for optical centre. No rim, no
  shadow. The whole glyph bbox sits inside the maskable safe zone (max corner
  radius 38.0% of the canvas; the zone is a centred circle 80% in diameter).
- `apple-touch-icon-180.png`: the same full-bleed art with the glyph at 56%.
  Opaque: no alpha channel (transparent corners would turn black under iOS).

The maskable and apple-touch files were composed as SVG (background rect plus
the source glyph paths, positioned from the glyph's measured bounding box) and
rasterised at exact size with headless Chrome screenshots (device scale 1).
