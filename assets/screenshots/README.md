# AI Account Center screenshot provenance

Every image here shows the real, compiled product with **sample data**. No
mockups, no image generation and no real accounts: every identity is an
`example.com` address, and no credential and no real or private path or
network address appears. The few that do show are samples: the project path
`~/projects/api` in `switch-review.png` and a truncated `10.0...` address
under Paired tray apps in `accounts.png`.

Captured 2026-10-16 from the build deployed that day (package 8.10.0, branch
`fix/analytics-agent-usage`, UI bundle `dist/ui/pkg/aaca825cbf64`).

## How each image was made

| Image | Source | Size |
| --- | --- | --- |
| `home-light.png`, `home-dark.png`, `analytics-*.png`, `accounts.png`, `switch-review.png` | The compiled Slint dashboard in Chrome on an NVIDIA GPU (WebGL2), 1600 CSS px wide at a real 2x device scale factor (`--force-device-scale-factor`, not emulation) | 3200 px wide |
| `pwa-phones.png` | The same dashboard at an iPhone viewport (402x874 CSS px, real 3x scale, touch, mobile user agent), framed with a drawn bezel and status bar | 4 screens |
| `tray-mac-*.png` | The installed Mac menu-bar app's own offline renderer (`CCSBar --render-preview`) | 760 pt panel at 2x |
| `tray-win-*.png` | The Windows tray's own offline renderer (`--render-fixture`) from a scratch build with the sample data embedded | 760 px panel at 2x |
| `demo.webp` | A 1920x1080 GPU recording (NVENC) of the dashboard, trimmed of loading frames, 1.5x speed, 12 fps, scaled to 1280 px | 28 s |

The `analytics-*.png` images are three crops of one full-page capture.

## The sample data

A local preview server served the compiled UI and answered its API calls from
generated data in the current response shapes: 15 accounts across all nine
providers, designed readings (one account near its weekly limit, Codex
auto-switch on, a recent automatic switch), 31 days of hourly CLI activity
priced with the product's own model rate table, a session sample, quota history
built from each window's reset period, and a finished Update apps run. The
trays rendered the same dashboard response. The guarded-switch dialog came
from the preview server answering a manual switch with the same one-use
confirmation the real server sends while Codex is busy; its process names and
PIDs are examples.

These images show what the interface looks like. They are not evidence of
live provider availability, deployed authentication or end-to-end behavior.
