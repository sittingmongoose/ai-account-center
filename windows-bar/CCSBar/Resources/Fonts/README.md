# Instrument Sans for the Windows tray

WPF does not read OpenType variation axes, so the tray embeds static faces cut from the official variable font
(google/fonts `ofl/instrumentsans/InstrumentSans[wdth,wght].ttf`, SIL Open Font License 1.1, see `OFL.txt`):

| File | Family (WPF) | Weight | Width |
| --- | --- | --- | --- |
| `InstrumentSans-Regular.ttf` | Instrument Sans | 400 | 100 |
| `InstrumentSans-Medium.ttf` | Instrument Sans | 500 | 100 |
| `InstrumentSans-SemiBold.ttf` | Instrument Sans | 600 | 100 |
| `InstrumentSans-Bold.ttf` | Instrument Sans | 700 | 100 |
| `InstrumentSansSemiCondensed-SemiBold.ttf` | Instrument Sans SemiCondensed | 600 | 87.5 (meter values) |

Every face has the `tnum` feature frozen into its default glyphs, so all digits and the percent sign share one advance
(tabular figures everywhere, with no dependence on renderer feature support). The `--check` suite verifies that each
face resolves to an embedded Instrument Sans glyph typeface at its own weight and that `0-9` and `%` are tabular.

Rebuild (Python venv with `fonttools` and `opentype-feature-freezer`):

```sh
python3 -m venv venv && ./venv/bin/pip install fonttools opentype-feature-freezer
curl -Lo InstrumentSans-VF.ttf "https://github.com/google/fonts/raw/main/ofl/instrumentsans/InstrumentSans%5Bwdth,wght%5D.ttf"
./venv/bin/python ../../../scripts/build-fonts.py   # writes the five TTFs next to the variable font
```
