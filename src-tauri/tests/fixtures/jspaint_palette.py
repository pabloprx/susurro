"""Synthetic JS Paint window for command.rs `jev_box_finds_red_in_a_synthetic_palette`.
1200 x 800 pt at 2x (2400 x 1600 px): Edge tab strip + address bar, the JS Paint menu bar, tools on the left,
a white canvas, the MS Paint palette at the bottom (2 rows x 14, 16 pt swatches, 17 pt pitch, x 38, rows y 733 / 750).
Red is row 2, column 3. Run: python3 jspaint_palette.py (writes jspaint_palette.png next to it)."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

S = 2
TOP = [(0, 0, 0), (128, 128, 128), (128, 0, 0), (128, 128, 0), (0, 128, 0), (0, 128, 128), (0, 0, 128), (128, 0, 128),
       (128, 128, 64), (0, 64, 64), (0, 128, 255), (0, 64, 128), (128, 0, 255), (128, 64, 0)]
BOTTOM = [(255, 255, 255), (192, 192, 192), (255, 0, 0), (255, 255, 0), (0, 255, 0), (0, 255, 255), (0, 0, 255), (255, 0, 255),
          (255, 255, 128), (0, 255, 128), (128, 255, 255), (128, 128, 255), (255, 0, 128), (255, 128, 64)]

img = Image.new("RGB", (1200 * S, 800 * S), (192, 192, 192))
d = ImageDraw.Draw(img)
font = ImageFont.truetype("/System/Library/Fonts/Helvetica.ttc", 12 * S)
r = lambda x, y, w, h, **k: d.rectangle([x * S, y * S, (x + w) * S - 1, (y + h) * S - 1], **k)
r(0, 0, 1200, 36, fill=(230, 230, 230))
r(8, 6, 180, 26, fill=(255, 255, 255)); d.text((16 * S, 12 * S), "JS Paint", fill=(0, 0, 0), font=font)
d.text((196 * S, 10 * S), "+", fill=(0, 0, 0), font=font)
r(0, 36, 1200, 36, fill=(250, 250, 250)); r(90, 42, 800, 24, fill=(255, 255, 255), outline=(180, 180, 180))
d.text((100 * S, 47 * S), "jspaint.app", fill=(60, 60, 60), font=font)
for i, m in enumerate(["File", "Edit", "View", "Image", "Colors", "Help"]):
    d.text(((8 + 52 * i) * S, 78 * S), m, fill=(0, 0, 0), font=font)
for i in range(16):
    r(4 + 26 * (i % 2), 100 + 26 * (i // 2), 24, 24, fill=(210, 210, 210), outline=(128, 128, 128))
r(70, 100, 1110, 610, fill=(255, 255, 255))
r(0, 726, 1200, 48, fill=(192, 192, 192))
r(4, 733, 30, 33, fill=(255, 255, 255), outline=(0, 0, 0)); r(10, 739, 12, 12, fill=(0, 0, 0))
for row, colors in ((733, TOP), (750, BOTTOM)):
    for i, c in enumerate(colors):
        r(38 + 17 * i, row, 16, 16, fill=c, outline=(128, 128, 128))
r(0, 780, 1200, 20, fill=(200, 200, 200)); d.text((6 * S, 783 * S), "For Help, click Help Topics on the Help Menu.", fill=(0, 0, 0), font=font)
img.save(Path(__file__).with_suffix(".png"))
