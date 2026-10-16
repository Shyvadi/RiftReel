"""Draws RiftReel's icon set into src-tauri/icons (original artwork).

The mark: a white dragon's head in profile, horns swept back, whose eye is a play
button, on a rounded tile that runs from teal to blue. The same mark is used at every
size, the tray icon (tray.png) included. Everything is drawn at 4x and scaled down, so
edges stay clean. src-tauri/icons/source.svg and ui/src/lib/Logo.svelte draw the same
paths.

usage: python tools/make-icons.py   (needs Pillow)
"""
import io
import re
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps

ICONS = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
SIZE = 1024
SUPER = 4
TEAL = (20, 184, 166)
BLUE = (37, 99, 235)

# The head, from the brow along the snout, the mouth (with a fang), the jaw and its
# spike, the frill, and the two horns. SVG path data on the 1024 grid.
HEAD = (
    "M517 430 L572 417 C617 435 657 465 697 490 C727 505 752 513 787 513 L832 575 "
    "L782 583.6 L767 611 L752 588.8 L647 607 L802 625 C777 655 737 675 697 685 "
    "C637 703 577 715 557 715 L417 725 L287 800 L377 675 L252 645 L362 595 L242 540 "
    "L347 505 C297 475 237 435 192 375 C267 385 347 405 397 440 "
    "C372 365 327 285 282 220 C377 250 467 325 502 420 Z"
)
NOSTRIL = "M757 535 C767 529 777 529 787 535 C777 541 767 541 757 535 Z"
# The eye: a play button, with round corners of radius 12.
EYE = [(542, 465), (542, 550), (615, 507)]


def tile(scale):
    """The rounded teal-to-blue tile, as RGBA at SIZE * scale."""
    n = SIZE * scale
    down = Image.linear_gradient("L").resize((n, n), Image.Resampling.BILINEAR)
    diagonal = ImageChops.add(down, down.transpose(Image.Transpose.TRANSPOSE), scale=2)
    image = ImageOps.colorize(diagonal, black=TEAL, white=BLUE).convert("RGBA")
    mask = Image.new("L", (n, n), 0)
    margin = 48 * scale
    ImageDraw.Draw(mask).rounded_rectangle(
        [margin, margin, n - margin, n - margin], radius=208 * scale, fill=255
    )
    image.putalpha(mask)
    return image


def rounded_triangle(draw, points, radius, scale):
    """A triangle with round corners: its outline drawn thick around a filled inset."""
    pts = [(x * scale, y * scale) for x, y in points]
    cx = sum(x for x, _ in pts) / 3
    cy = sum(y for _, y in pts) / 3
    r = radius * scale
    inset = []
    for x, y in pts:
        dx, dy = cx - x, cy - y
        length = (dx * dx + dy * dy) ** 0.5
        # Move each corner toward the middle so the round corner meets the old edges.
        inset.append((x + dx / length * r * 2, y + dy / length * r * 2))
    draw.polygon(inset, fill=255)
    draw.line(inset + [inset[0]], fill=255, width=int(2 * r), joint="curve")
    for x, y in inset:
        draw.ellipse([x - r, y - r, x + r, y + r], fill=255)


def outline(path, scale):
    """The points of one closed SVG path of absolute M, L and C commands, curves sampled."""
    points = []
    for command, args in re.findall(r"([MLCZ])([^MLCZ]*)", path):
        values = [float(v) * scale for v in args.split()]
        if command in "ML":
            points += zip(values[0::2], values[1::2])
        elif command == "C":
            for i in range(0, len(values), 6):
                (x0, y0), (x1, y1, x2, y2, x3, y3) = points[-1], values[i : i + 6]
                for step in range(1, 33):
                    t = step / 32
                    a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t**3
                    points.append((a * x0 + b * x1 + c * x2 + d * x3, a * y0 + b * y1 + c * y2 + d * y3))
    return points


def dragon(scale):
    """The white dragon's head with its play-button eye, as an alpha mask."""
    n = SIZE * scale
    mask = Image.new("L", (n, n), 0)
    draw = ImageDraw.Draw(mask)
    draw.polygon(outline(HEAD, scale), fill=255)
    draw.polygon(outline(NOSTRIL, scale), fill=0)
    eye = Image.new("L", (n, n), 0)
    rounded_triangle(ImageDraw.Draw(eye), EYE, 12, scale)
    return ImageChops.subtract(mask, eye)


def compose(glyph):
    """The tile with a white glyph and its soft shadow, at SIZE * SUPER."""
    base = tile(SUPER)
    shadow = Image.new("RGBA", base.size, (8, 24, 48, 0))
    shadow.putalpha(glyph.point(lambda v: int(v * 0.35)).filter(ImageFilter.GaussianBlur(14 * SUPER)))
    shadow = ImageChops.offset(shadow, 0, 12 * SUPER)
    tile_alpha = base.getchannel("A")
    shadow.putalpha(ImageChops.multiply(shadow.getchannel("A"), tile_alpha))
    base = Image.alpha_composite(base, shadow)
    white = Image.new("RGBA", base.size, (255, 255, 255, 0))
    white.putalpha(glyph)
    return Image.alpha_composite(base, white)


FULL = compose(dragon(SUPER))


def render(size):
    return FULL.resize((size, size), Image.Resampling.LANCZOS)


def png(size):
    data = io.BytesIO()
    render(size).save(data, "PNG", optimize=True)
    return data.getvalue()


def write_ico(path, sizes):
    """An ICO of PNG entries (Windows Vista and later read them at every size)."""
    images = [png(size) for size in sizes]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    directory = b""
    for size, data in zip(sizes, images):
        side = 0 if size >= 256 else size
        directory += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path.write_bytes(header + directory + b"".join(images))


def main():
    outputs = {
        "32x32.png": 32,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "icon.png": 512,
        "Square30x30Logo.png": 30,
        "Square44x44Logo.png": 44,
        "Square71x71Logo.png": 71,
        "Square89x89Logo.png": 89,
        "Square107x107Logo.png": 107,
        "Square142x142Logo.png": 142,
        "Square150x150Logo.png": 150,
        "Square284x284Logo.png": 284,
        "Square310x310Logo.png": 310,
        "StoreLogo.png": 50,
        # The tray icon, shown at 16 to 32 px.
        "tray.png": 32,
    }
    for name, size in outputs.items():
        (ICONS / name).write_bytes(png(size))
    write_ico(ICONS / "icon.ico", [16, 20, 24, 32, 40, 48, 64, 128, 256])
    print(f"wrote {len(outputs) + 1} icons to {ICONS}")


if __name__ == "__main__":
    main()
