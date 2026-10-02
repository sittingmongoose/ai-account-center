# Builds CCSBar/Resources/Icons/AppIcon.ico, the Windows app icon (exe, window, Start menu and Desktop shortcuts),
# from the approved Apex Soft app icon exports (the light plate, the same art as the Mac AppIcon-source.png).
# The plate keeps the mark legible on any wallpaper, Start menu or taskbar; the bare TrayLight/TrayDark glyphs stay
# notification-area art only. Usage: python3 build-app-icon.py <logos/export folder> <output .ico>
# Needs Pillow only for the sizes the export does not ship (20, 24, 40), resized from the 1024 px master.
import io, struct, sys
from pathlib import Path
from PIL import Image

export, out = Path(sys.argv[1]), Path(sys.argv[2])
SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
master = Image.open(export / 'appicon-light-1024.png').convert('RGBA')
frames = []
for size in SIZES:
    shipped = export / f'appicon-light-{size}.png'
    image = Image.open(shipped).convert('RGBA') if shipped.exists() else master.resize((size, size), Image.LANCZOS)
    assert image.size == (size, size), shipped
    data = io.BytesIO(); image.save(data, format='PNG', optimize=True)
    frames.append((size, data.getvalue()))
header = struct.pack('<HHH', 0, 1, len(frames))
offset = 6 + 16 * len(frames)
entries, blobs = b'', b''
for size, blob in frames:
    entries += struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(blob), offset + len(blobs))
    blobs += blob
out.write_bytes(header + entries + blobs)
print(f'{out}: {", ".join(str(size) for size, _ in frames)} px')
