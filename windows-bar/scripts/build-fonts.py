# Builds the static, tabular Instrument Sans faces embedded in the Windows tray (see CCSBar/Resources/Fonts/README.md).
# Run from a folder holding InstrumentSans-VF.ttf, with fonttools and opentype-feature-freezer installed.
import subprocess, sys
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

SRC = 'InstrumentSans-VF.ttf'
faces = [
    ('InstrumentSans-Regular.ttf', 'Instrument Sans', 'Regular', 400, 100, 5),
    ('InstrumentSans-Medium.ttf', 'Instrument Sans', 'Medium', 500, 100, 5),
    ('InstrumentSans-SemiBold.ttf', 'Instrument Sans', 'SemiBold', 600, 100, 5),
    ('InstrumentSans-Bold.ttf', 'Instrument Sans', 'Bold', 700, 100, 5),
    ('InstrumentSansSemiCondensed-SemiBold.ttf', 'Instrument Sans SemiCondensed', 'SemiBold', 600, 87.5, 4),
]
for out, family, style, wght, wdth, width_class in faces:
    vf = TTFont(SRC)
    f = instancer.instantiateVariableFont(vf, {'wght': wght, 'wdth': wdth}, updateFontNames=False)
    tmp = out + '.tmp.ttf'
    f.save(tmp)
    # Freeze the tnum feature so every digit is tabular without relying on renderer feature support.
    subprocess.run([sys.prefix + '/bin/pyftfeatfreeze', '-f', 'tnum', tmp, out], check=True, capture_output=True)
    f = TTFont(out)
    name = f['name']
    ribbi = style in ('Regular', 'Bold')
    legacy_family = family if ribbi else f'{family} {style}'
    legacy_style = style if ribbi else 'Regular'
    full = f'{family} {style}'
    ps = (family.replace(' ', '') + '-' + style)
    for rec in list(name.names):
        if rec.nameID in (1, 2, 3, 4, 6, 16, 17, 25) or rec.nameID >= 256:
            name.removeNames(nameID=rec.nameID)
    for nid, val in ((1, legacy_family), (2, legacy_style), (3, ps + ';AAC-tnum'), (4, full), (6, ps), (16, family), (17, style)):
        name.setName(val, nid, 3, 1, 0x409)
        name.setName(val, nid, 1, 0, 0)
    os2 = f['OS/2']
    os2.usWeightClass = wght
    os2.usWidthClass = width_class
    sel = os2.fsSelection & ~0b1100001  # clear italic, bold, regular
    if style == 'Bold': sel |= 0b100000
    elif style == 'Regular': sel |= 0b1000000
    os2.fsSelection = sel
    f['head'].macStyle = 1 if style == 'Bold' else 0
    for t in ('STAT', 'fvar', 'gvar', 'avar', 'HVAR', 'MVAR'):
        if t in f: del f[t]
    f.save(out)
    import os; os.remove(tmp)
    # verify digits share one advance
    g = TTFont(out); cmap = g.getBestCmap(); hm = g['hmtx']
    adv = {hm[cmap[ord(c)]][0] for c in '0123456789'}
    print(out, wght, wdth, 'digit advances', adv)
