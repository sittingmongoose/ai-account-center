# PWA icons (DESIGN-MOBILE.md 6.4)

Derived from the dark Apex artwork (`appicon-dark.svg` /
`appicon-dark-1024.png`: dark squircle body, rim, soft shadow, transparent
corners; same design as the Mac menu-bar `AppIconDark`).

- `aac-192.png`, `aac-512.png` (`purpose: any`): the 1024 px export scaled with
  `ffmpeg -vf scale=W:H:flags=lanczos -sws_dither ed`. Transparent corners kept.
- `aac-maskable-192.png`, `aac-maskable-512.png` (`purpose: maskable`): full-bleed
  square filled with the dark icon body's own gradient (`#2C3746` to `#19212B`
  at 50% to `#0D1218`, top to bottom). The Apex glyph (light legs plus blue bar
  plus the bar gloss, same gradients, taken as the `<g>` paths from
  `appicon-dark.svg`) is centred with its true bounding box (apex arc included)
  at 52% of the canvas height, nudged down 1% for optical centre. No rim, no
  shadow, no glow. The whole glyph bbox sits inside the maskable safe zone
  (max corner radius 38.0% of the canvas; the zone is a centred circle 80% in
  diameter).
- `apple-touch-icon-180.png`: the same full-bleed art with the glyph at 56%.
  Opaque: no alpha channel (transparent corners would turn black under iOS).

The maskable and apple-touch files were composed as SVG (background rect plus
the source glyph paths, positioned from the glyph's measured bounding box) and
rasterised at exact size with headless Chrome screenshots (device scale 1).
