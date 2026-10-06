#!/usr/bin/env python3
"""Build public/assets/AACMask.ttf, the password mask dot of the sign-in page's HTML password input.

Slint draws a masked password with U+25CF BLACK CIRCLE (i-slint-core 1.18.1 PASSWORD_CHARACTER). The dashboard's own
fonts lack that glyph, so in the browser Slint takes it from its embedded fallback font, Inter
(i-slint-common 1.18.1 sharedfontique/Inter-VariableFont.ttf, Copyright 2020 The Inter Project Authors,
SIL OFL 1.1 with a Reserved Font Name). Browsers mask a password input with U+2022 (Chrome, Edge, Brave, Firefox) or
U+25CF (Safari), so this font maps both to that one Inter glyph, pinned at weight 400 as Slint draws it, and holds
nothing else. index.html uses it for those two code points only (unicode-range), so the HTML input's dots look like
the Slint field's. Renamed "AAC Mask" because the licence reserves the original name.

Usage: python3 build_mask_font.py <path to Inter-VariableFont.ttf>   (needs fonttools)
"""
import os
import sys
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from fontTools import subset

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "..", "public", "assets", "AACMask.ttf")
FAMILY = "AAC Mask"


def main(src):
    font = TTFont(src)
    if "fvar" in font:
        font = instancer.instantiateVariableFont(font, {axis.axisTag: 400 if axis.axisTag == "wght" else axis.defaultValue for axis in font["fvar"].axes})
    for table in ("STAT", "GSUB", "GPOS", "GDEF"):
        if table in font:
            del font[table]
    opts = subset.Options()
    opts.layout_features = []
    opts.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
    opts.notdef_outline = True
    opts.glyph_names = False
    opts.hinting = False
    sub = subset.Subsetter(options=opts)
    sub.populate(unicodes=[0x25CF])
    sub.subset(font)
    for table in font["cmap"].tables:
        if 0x25CF in table.cmap:
            table.cmap[0x2022] = table.cmap[0x25CF]
    ps = FAMILY.replace(" ", "") + "-Regular"
    for rec in font["name"].names:
        if rec.nameID == 1:
            rec.string = FAMILY
        elif rec.nameID == 2:
            rec.string = "Regular"
        elif rec.nameID == 3:
            rec.string = f"{ps};aac-dashboard"
        elif rec.nameID == 4:
            rec.string = FAMILY
        elif rec.nameID == 6:
            rec.string = ps
    font.save(OUT)
    print(os.path.normpath(OUT), os.path.getsize(OUT))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(sys.argv[1])
