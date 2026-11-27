<!-- quickstart-snippet-start -->
## Quick Start (Docker)

From a checkout of [AI Account Center](https://github.com/sittingmongoose/ai-account-center) with a gated Linux release tarball (`sittingmongoose-ai-account-center-8.10.0.tgz`):

```bash
AAC_RELEASE_DIR=/path/to/release/linux AAC_BUILD_COMMIT=<40-hex-release-sha> docker compose -f docker/compose.yaml up -d --build
```

Open `http://localhost:3000`. This packages the gated release tarball (Rust
dashboard, Leptos UI, pinned cross-OS agents, scanners, Cursor readers, and
`dist/build-info.json`) into a minimal Debian runtime with no Node, Bun, or
Python interpreter; it does not download the upstream CCS package or start a
CLIProxy service. `AAC_BUILD_COMMIT` must match the release tarball's
compiled commit and `dist/build-info.json`.
Existing host credentials and usage-collector connections require explicit
configuration; see the Docker deployment guide in this checkout.
<!-- quickstart-snippet-end -->
