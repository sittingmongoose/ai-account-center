#!/usr/bin/env python3
"""Rebuild the dashboard's embedded fonts from the upstream Google Fonts variable TTFs.

Inputs (downloaded from github.com/google/fonts, SIL OFL 1.1, licence texts kept next to the outputs):
  ofl/instrumentsans/InstrumentSans[wdth,wght].ttf
  ofl/martianmono/MartianMono[wdth,wght].ttf

Outputs (in this folder):
  InstrumentSans.ttf     family "Instrument Sans",     wdth pinned at 100, wght 400..700 kept variable
  InstrumentSansTab.ttf  family "Instrument Sans Tab", the same with the OpenType `tnum` feature frozen
                         into the cmap, so every digit is tabular without OpenType feature support
                         (Slint 1.18.1 has no font-feature property)
  MartianMono.ttf        family "Martian Mono",        wdth pinned at 87.5 (the concept's caps width), wght variable

All three are subset to Latin, Latin-1 and common punctuation to keep the WebAssembly bundle small.
Neither upstream licence declares a Reserved Font Name; the Tab copy is renamed anyway so it can
never be confused with the original.

Usage: python3 build_fonts.py <dir with the upstream files>   (needs fonttools and opentype-feature-freezer)
"""
import subprocess, sys, os, shutil, tempfile
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
from fontTools import subset

HERE = os.path.dirname(os.path.abspath(__file__))
UNICODES = (
    "U+0020-007E,U+00A0-00FF,U+0131,U+0152-0153,U+02C6,U+02DA,U+02DC,"
    "U+2000-206F,U+20AC,U+2122,U+2212,U+2215,U+2264-2265,U+FEFF"
)


def rename(font, family, style="Regular"):
    name = font["name"]
    full = family if style == "Regular" else f"{family} {style}"
    ps = (family + "-" + style).replace(" ", "")
    for rec in list(name.names):
        if rec.nameID in (1, 16):
            rec.string = family
        elif rec.nameID in (2, 17):
            rec.string = style
        elif rec.nameID == 4:
            rec.string = full
        elif rec.nameID == 6:
            rec.string = ps
        elif rec.nameID == 3:
            rec.string = f"{ps};aac-dashboard"
        elif rec.nameID == 25:
            rec.string = ps.replace("-Regular", "")


def pin(src, axes, family, out):
    font = TTFont(src)
    font = instancer.instantiateVariableFont(font, axes)
    if "STAT" in font:
        del font["STAT"]
    rename(font, family)
    font.save(out)


def do_subset(path):
    opts = subset.Options()
    opts.layout_features = ["*"]
    opts.name_IDs = ["*"]
    opts.name_languages = ["*"]
    opts.notdef_outline = True
    opts.glyph_names = False
    font = TTFont(path)
    sub = subset.Subsetter(options=opts)
    sub.populate(unicodes=subset.parse_unicodes(UNICODES))
    sub.subset(font)
    font.save(path)


def main(src_dir):
    tmp = tempfile.mkdtemp()
    try:
        inst = os.path.join(src_dir, "instrumentsans-InstrumentSans[wdth,wght].ttf")
        mono = os.path.join(src_dir, "martianmono-MartianMono[wdth,wght].ttf")
        base = os.path.join(HERE, "InstrumentSans.ttf")
        pin(inst, {"wdth": 100}, "Instrument Sans", base)
        tab_tmp = os.path.join(tmp, "tab.ttf")
        subprocess.run([shutil.which("pyftfeatfreeze") or "pyftfeatfreeze", "-f", "tnum", base, tab_tmp], check=True)
        tab = TTFont(tab_tmp)
        rename(tab, "Instrument Sans Tab")
        tab.save(os.path.join(HERE, "InstrumentSansTab.ttf"))
        pin(mono, {"wdth": 87.5}, "Martian Mono", os.path.join(HERE, "MartianMono.ttf"))
        for name in ("InstrumentSans.ttf", "InstrumentSansTab.ttf", "MartianMono.ttf"):
            do_subset(os.path.join(HERE, name))
            print(name, os.path.getsize(os.path.join(HERE, name)))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else os.getcwd())
