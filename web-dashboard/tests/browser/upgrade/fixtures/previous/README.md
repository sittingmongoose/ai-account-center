# The previous release, as installed devices hold it

- `sw.js`, `sw-route.js`: the service worker of `f52e1a7c6` (the first build without the classic app, installed
  2026-11-14), byte for byte. Its code is the same as every earlier release's since the worker shipped; only comments
  differ. The fixture server stamps it with an older build id.
- `classic-index.html`: the classic start page of `df371d05b`, the last build that had one. A device that chose the
  classic app (`aac-frame=classic`) may still hold it in that worker's cache. It loads one script, `/bridge.js`.

Never edit these: they are what a real device upgrades from.
