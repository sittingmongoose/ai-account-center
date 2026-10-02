# Provider artwork sources

Official provider artwork identifies each provider's accounts. It is shown unmodified: no recolouring, redrawing or geometry changes. The PNGs are 256 px renders of the official vector artwork, produced by the AI Account Center redesign marks pipeline (`shared/marks-v2`, `sources.json` lists every source file and its SHA-256). A `-light` file is the provider's own light-surface artwork, used in the Light appearance.

## claude

Source: Anthropic press kit, "Claude Spark - Clay.svg" (https://www.anthropic.com/press-kit). One artwork for light and dark surfaces.

`provider-claude.png` SHA-256: `12fe263111fc8e0e975a14b7c3a2bf4b0d9eacea88ea311f96a92a9ad0b6b479`

Claude and the Claude Spark are Anthropic trademarks.

## codex

Source: OpenAI brand pack (https://cdn.openai.com/brand/openai-logos.zip), OAI_OpenAI-Blossom_White.svg (dark surfaces) and _Black.svg (light surfaces). Codex publishes no separate product mark, so the OpenAI Blossom identifies it.

`provider-codex.png` SHA-256: `b280c0b5bdc32daff7f01e59bd29a7be587c11fbd4a1939cab924626ed5a2d73`
`provider-codex-light.png` SHA-256: `f57f90c8de55d12e594101a25c0886c33eb2064cb6526613567d4dbb5bcf4493`

OpenAI and the Blossom are OpenAI trademarks.

## antigravity

Source: Header lockup SVG on https://antigravity.google/, cropped to its symbol (wordmark paths outside the crop dropped; render verified pixel-identical).

`provider-antigravity.png` SHA-256: `f6e19710e607896b875a08f6c240d8dfdf338463353268274fd0207fa2ff0a15`

Google Antigravity is a Google trademark.

## cursor

Source: Cursor brand assets (https://cursor.com/brand), General Logos/Cube/SVG/CUBE_2D_DARK.svg (dark surfaces) and CUBE_2D_LIGHT.svg (light surfaces).

`provider-cursor.png` SHA-256: `3210b5817ca5203669b1a8303e50a5c98e76ce5900e9ed2265ded6a8f6af8446`
`provider-cursor-light.png` SHA-256: `2f2e1d62a7d42ddd8e4e3ded3619ce1e272393b7561e52d904644c83c328ee7d`

Cursor is an Anysphere trademark.

## muse

Source: Meta lockup preloaded by https://dev.meta.ai/products/muse-code, symbol cropped out. The Muse Code page publishes no separate product mark, so Meta's publisher symbol identifies it.

`provider-muse.png` SHA-256: `d96327607162cece5efbdb88f83bb1693df041ff0d2220322d7ca2f204fd707e`

Meta and the Meta symbol are Meta Platforms trademarks.

## kimi-code

Source: Kimi Code docs logo, the official Kimi app icon with its black rounded plate (https://raw.githubusercontent.com/MoonshotAI/kimi-code/21406fb4c805cc8c715e6d1f16ad3fb5f25f4fe3/docs/.vitepress/theme/Kimi.png). Used on both surfaces at the owner's request.

`provider-kimi-code.png` SHA-256: `c367897f22e26c917200dc6bd5e5cb1456d24bf5f6cf8dcb1524f9d34e336b6d`

Kimi is a Moonshot AI trademark.

## qwen

Source: Qwen Cloud docs logos (https://docs.qwencloud.com/logo/dark.svg and light.svg), symbol cropped from the lockup.

`provider-qwen.png` SHA-256: `ad378d712faf5c4f4bfbf01459e41552fb07a14efb9854f6aa5b3eec92432e0d`
`provider-qwen-light.png` SHA-256: `aff0b6cdf8772aca3a236ff17dc46a1cbab0767de5c503f66cc8262e8d3c21bd`

Qwen and Qwen Cloud are Alibaba Cloud trademarks.

## zai

Source: chat.z.ai logos (https://chat.z.ai/static/logoDark.svg and https://z-cdn.chatglm.cn/z-ai/static/logoLight.svg).

`provider-zai.png` SHA-256: `eccb0d10af965b6dcdc6f8fb299758886495ea43d4a08215a1c687cd9a92ae25`
`provider-zai-light.png` SHA-256: `edcaff14f9043c7ab4de26bfaff67451dd380e90d4dccd96874887080fde2c02`

Z.ai is a Zhipu AI trademark.

## opencode-go

Source: OpenCode brand page SVGs (anomalyco/opencode commit aa481b8f5652f5576c55f914a64ed270e7daa7e0, packages/console/app/src/asset/brand/opencode-logo-dark.svg and opencode-logo-light.svg).

`provider-opencode-go.png` SHA-256: `22dace667cd35ab4534214c73a0ba6a3061a41c9c8e5dc8fb36447a628cd7635`
`provider-opencode-go-light.png` SHA-256: `c74d15a45f5f87d84e5688e1a1bb945f62454c016a51537edf8f8bffbcf85b0d`

OpenCode marks belong to the OpenCode project (code under MIT).

## platform-apple

Source: Simple Icons 16.33.0 apple.svg (https://cdn.jsdelivr.net/npm/simple-icons@16.33.0/icons/apple.svg), CC0-1.0. Shown as a template glyph tinted by the system.

`platform-apple.png` SHA-256: `edcc7f21a9b46c1829ca39c24658bbc0b139849a45fe1b7afe869e7046190a9b`

The Apple logo is an Apple Inc. trademark, used only as a platform indicator.

## platform-windows

Source: Simple Icons 12.4.0 windows.svg (https://cdn.jsdelivr.net/npm/simple-icons@12.4.0/icons/windows.svg), CC0-1.0. Shown as a template glyph tinted by the system.

`platform-windows.png` SHA-256: `fd67df38b268f16005f262892ae4401d7b63c92a1e4712f95e67f29223a897a1`

The Windows logo is a Microsoft trademark, used only as a platform indicator.

## AI Account Center (Apex Soft)

`MenuBarTemplate.png`, `MenuBarTemplate@2x.png`, `ApexInk.svg`, `ApexMeter.svg`, `AppIconLight.png`, `AppIconDark.png` and `../AppIcon-source.png` come from the project's own Apex Soft logo export (redesign `logos/export`, built by `logos/build_logos.py`).

The complete attribution and the OpenCode MIT notice are in `THIRD-PARTY-NOTICES.txt`, which is bundled with the app.
