# Provider artwork sources

Official provider artwork from the AI Account Center marks set (`redesign-concepts-20261001/shared/marks-v2`, generated 2026-10-01T20:31:31.632Z).
Rules: Official artwork only. No recolouring, redrawing or geometry changes. SVG edits are limited to dropping the XML prolog and editor comments, setting the root viewBox/width/height (the crop), and, for lockups, dropping wordmark elements that lie wholly outside the crop (each verified pixel-identical). Optical scale is metadata only.

The PNGs here are the marks set's 256 px exports (`out/png/<id>@256.png`); `<id>-light.png` is the official light-surface artwork, used in the Light theme. Kimi Code uses the official Kimi Code app icon with its own black plate (the owner's choice). Optical scale is metadata only: the tray draws each mark in its box at box x scale.

## claude

One artwork for both surfaces: the press kit ships the Spark only in Clay (#D97757), which reads on light and dark.

Claude and the Claude Spark are Anthropic trademarks. Official press-kit artwork, shown unmodified to identify the provider.

- Source: https://www.anthropic.com/press-kit (Anthropic media resources zip (sha256 c68ac92df86c825f95177e24016fcc9a8863a3fd4ca344fe6f0700b2c1e07151), member "Anthropic media resources/Anthropic logos/Claude logos/3 Claude Spark/SVG/Claude Spark - Clay.svg")
- Source: https://claude.ai/favicon.svg (same Spark geometry, kept for reference only)
- `claude.png` SHA-256 `12fe263111fc8e0e975a14b7c3a2bf4b0d9eacea88ea311f96a92a9ad0b6b479`

## codex

Official black Blossom on light surfaces, official white Blossom on dark surfaces.

OpenAI and the Blossom are OpenAI trademarks. Official brand-pack artwork, unmodified. Codex publishes no separate product mark (developers.openai.com/codex uses only UI icons), so the OpenAI Blossom identifies it.

- Source: https://cdn.openai.com/brand/openai-logos.zip (OpenAI brand logo pack (zip sha256 c54e85ab5884228f89f0230dd8effa8d588cad78166fe954135f4afa553222db), members OpenAI-logos/SVGs/OAI_OpenAI-Blossom_White.svg and _Black.svg; identical geometry, fill white vs black)
- `codex.png` SHA-256 `b280c0b5bdc32daff7f01e59bd29a7be587c11fbd4a1939cab924626ed5a2d73`
- `codex-light.png` SHA-256 `f57f90c8de55d12e594101a25c0886c33eb2064cb6526613567d4dbb5bcf4493`

## antigravity

One artwork for both surfaces (multicolour gradient, reads on light and dark). Not used: /assets/image/antigravity-logo.svg on the same site is a different, stepped-pixel "A" (the CLI splash art), not the site icon.

Google Antigravity is a Google trademark. Official vector artwork from antigravity.google, unmodified; the lockup is cropped to its symbol and the wordmark paths that fall outside the crop are dropped (render verified pixel-identical).

- Source: https://antigravity.google/ (inline header lockup SVG (viewBox 0 0 869 113) from the homepage HTML (saved as antigravity-home.html); the arch symbol is the masked group at x 13..98. Same mark as the site icon /assets/image/antigravity-logo.png (200x184) and /apple-touch-icon.png (180))
- `antigravity.png` SHA-256 `f6e19710e607896b875a08f6c240d8dfdf338463353268274fd0207fa2ff0a15`

## cursor

CUBE_2D_LIGHT (dark ink) on light surfaces, CUBE_2D_DARK (light ink) on dark surfaces.

Cursor is an Anysphere trademark. Official brand-kit SVGs, unmodified.

- Source: https://ptht05hbb1ssoooe.public.blob.vercel-storage.com/assets/brand/cursor-brand-assets.zip (Cursor brand assets (linked from cursor.com/brand), zip sha256 97488a7751914e60f9ff532bc33810cdeaebdddc017548abe6ca2bc29bbc3928, members "General Logos/Cube/SVG/CUBE_2D_DARK.svg" (fill #edecec) and "CUBE_2D_LIGHT.svg" (fill #26251e))
- `cursor.png` SHA-256 `3210b5817ca5203669b1a8303e50a5c98e76ce5900e9ed2265ded6a8f6af8446`
- `cursor-light.png` SHA-256 `2f2e1d62a7d42ddd8e4e3ded3619ce1e272393b7561e52d904644c83c328ee7d`

## muse

One artwork for both surfaces (Meta blue gradient reads on light and dark).

Meta and the Meta symbol are Meta Platforms trademarks. The Muse Code product page publishes no separate Muse Code mark (its only images are the Meta lockups, a 64px favicon of the same symbol and a background), so Meta's publisher symbol identifies it. Vector, unmodified; wordmark paths outside the crop dropped (pixel-identical render).

- Source: https://dev.meta.ai/logo/meta-logo-with-text.svg (Meta lockup preloaded by https://dev.meta.ai/products/muse-code; symbol cropped out (same symbol in -dark.svg, whose only change is a white wordmark))
- `muse.png` SHA-256 `d96327607162cece5efbdb88f83bb1693df041ff0d2220322d7ca2f204fd707e`

## kimi-code

Black K on light surfaces, white K on dark surfaces.

Kimi is a Moonshot AI trademark. Kimi publishes its colour K (white K, blue dot) only as an app icon on a black plate (kimi.com pwa-192/512, favicons, Kimi Code docs and VS Code extension). The bare mark here is Kimi's own web-UI glyph, geometry unmodified; it is a currentColor glyph, rendered in the black/white pair Kimi uses for its official KIMI wordmark on light and dark. No blue dot exists in any bare official artwork.

- Source: https://statics.moonshot.cn/kimi-web-seo/assets/kimi.icon-CElMvu4q.js (kimi.com web app icon collection, glyph "KforKimi_f" (the K with its dot, 1024 grid, fill=currentColor). Extracted to kimi-web-iconset.json)
- Source: https://platform.kimi.com/ (official KIMI wordmark ships as logo-image-light fill #000 and logo-image-dark fill #fff; the same black/white pair is used for the K glyph)
- Source: https://raw.githubusercontent.com/MoonshotAI/kimi-code/21406fb4c805cc8c715e6d1f16ad3fb5f25f4fe3/docs/.vitepress/theme/Kimi.png (Kimi Code docs logo = the app icon (black rounded plate, white K, blue dot), not used because it is a plate)
- `kimi-code.png` SHA-256 `c367897f22e26c917200dc6bd5e5cb1456d24bf5f6cf8dcb1524f9d34e336b6d`

## qwen

Official gradient symbol on light surfaces, official white symbol on dark surfaces.

Qwen and Qwen Cloud are Alibaba Cloud trademarks. The Token Plan is a Qwen Cloud product (docs.qwencloud.com/token-plan; the collector reads home.qwencloud.com), so the Qwen Cloud symbol is used. Vector, unmodified; wordmark paths outside the crop dropped (pixel-identical render).

- Source: https://docs.qwencloud.com/logo/light.svg (Qwen Cloud docs nav logo for light mode (gradient #4F21FF to #D75BFE), symbol cropped from the lockup)
- Source: https://docs.qwencloud.com/logo/dark.svg (Qwen Cloud docs nav logo for dark mode (white), symbol cropped from the lockup)
- Source: https://img.alicdn.com/imgextra/i1/O1CN01OwlzsC1cRTnZrFfXa_!!6000000003597-2-tps-150-150.png (alternative not used: qwen.ai blue Qwen (model) symbol, 150px raster only)
- `qwen.png` SHA-256 `ad378d712faf5c4f4bfbf01459e41552fb07a14efb9854f6aa5b3eec92432e0d`
- `qwen-light.png` SHA-256 `aff0b6cdf8772aca3a236ff17dc46a1cbab0767de5c503f66cc8262e8d3c21bd`

## zai

Dark Z on light surfaces, white Z on dark surfaces.

Z.ai is a Zhipu AI trademark. Official bare Z artwork from chat.z.ai, vector, unmodified.

- Source: https://z-cdn.chatglm.cn/z-ai/static/logoLight.svg (chat.z.ai light-theme logo (dark gradient Z, #070606 to #3b3b3b); identical to https://chat.z.ai/static/logoLight.svg)
- Source: https://chat.z.ai/static/logoDark.svg (chat.z.ai dark-theme logo (white Z). chat.z.ai picks logoLight for theme "light" and logoDark otherwise)
- Source: https://mintcdn.com/zhipu-32152247/B_E8wI-eiNa1QlPV/logo/dark.svg (not used: docs.z.ai app-icon logo (white Z on a black rounded square plate))
- `zai.png` SHA-256 `eccb0d10af965b6dcdc6f8fb299758886495ea43d4a08215a1c687cd9a92ae25`
- `zai-light.png` SHA-256 `edcaff14f9043c7ab4de26bfaff67451dd380e90d4dccd96874887080fde2c02`

## opencode-go

logo-light (dark frame) on light surfaces, logo-dark (light frame) on dark surfaces.

OpenCode marks belong to the OpenCode project (anomalyco/opencode, code under MIT). Official brand-page SVGs, unmodified.

- Source: https://raw.githubusercontent.com/anomalyco/opencode/aa481b8f5652f5576c55f914a64ed270e7daa7e0/packages/console/app/src/asset/brand/opencode-logo-dark.svg (brand asset served on opencode.ai/brand; light frame #F1ECEC, for dark surfaces)
- Source: https://raw.githubusercontent.com/anomalyco/opencode/aa481b8f5652f5576c55f914a64ed270e7daa7e0/packages/console/app/src/asset/brand/opencode-logo-light.svg (dark frame #211E1E, for light surfaces)
- `opencode-go.png` SHA-256 `22dace667cd35ab4534214c73a0ba6a3061a41c9c8e5dc8fb36447a628cd7635`
- `opencode-go-light.png` SHA-256 `c74d15a45f5f87d84e5688e1a1bb945f62454c016a51537edf8f8bffbcf85b0d`
