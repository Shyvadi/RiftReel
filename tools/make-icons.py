"""Draws RiftReel's icon set into src-tauri/icons (original artwork).

The mark: a bold white "R" whose counter is a play button, on a rounded tile that runs
from teal to blue. At 20 px and under, where the letter would blur, the tile carries
just the play button, and so does the tray icon (tray.png). Everything is drawn at 4x and scaled down, so edges stay clean.

usage: python tools/make-icons.py   (needs Pillow)
"""
import io
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps

ICONS = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"
SIZE = 1024
SUPER = 4
TEAL = (20, 184, 166)
BLUE = (37, 99, 235)


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


def letter(scale):
    """The white R with its play-button counter, as an alpha mask."""
    n = SIZE * scale
    s = scale
    stem_left, stem_right, top, bottom = 275, 410, 263, 761
    centre_x, radius = 541, 172
    bowl_bottom = top + 2 * radius
    mask = Image.new("L", (n, n), 0)
    draw = ImageDraw.Draw(mask)
    # The stem, round on the left only.
    draw.rounded_rectangle(
        [stem_left * s, top * s, stem_right * s, bottom * s],
        radius=49 * s,
        fill=255,
        corners=(True, False, False, True),
    )
    # The leg, from inside the bowl down to the right.
    leg = [(454, 560), (612, 560), (741, 761), (583, 761)]
    draw.polygon([(x * s, y * s) for x, y in leg], fill=255)
    # The bowl: a bar from the stem that ends in the right half of a disc.
    bowl = Image.new("L", (n, n), 0)
    ImageDraw.Draw(bowl).ellipse(
        [(centre_x - radius) * s, top * s, (centre_x + radius) * s, bowl_bottom * s], fill=255
    )
    ImageDraw.Draw(bowl).rectangle([0, 0, centre_x * s, n], fill=0)
    ImageDraw.Draw(bowl).rectangle(
        [stem_right * s, top * s, centre_x * s, bowl_bottom * s], fill=255
    )
    mask = ImageChops.lighter(mask, bowl)
    # The counter: a play button.
    hole = Image.new("L", (n, n), 0)
    rounded_triangle(ImageDraw.Draw(hole), [(430, 356), (430, 514), (567, 435)], 22, s)
    return ImageChops.subtract(mask, hole)


def play(scale):
    """A white play button alone, for the smallest sizes."""
    n = SIZE * scale
    mask = Image.new("L", (n, n), 0)
    rounded_triangle(ImageDraw.Draw(mask), [(358, 257), (358, 767), (810, 512)], 46, scale)
    return mask


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


FULL = compose(letter(SUPER))
SMALL = compose(play(SUPER))


def render(size):
    source = SMALL if size <= 20 else FULL
    return source.resize((size, size), Image.Resampling.LANCZOS)


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
    }
    for name, size in outputs.items():
        (ICONS / name).write_bytes(png(size))
    # The tray icon: the play button alone, which stays sharp at 16 px.
    tray = io.BytesIO()
    SMALL.resize((32, 32), Image.Resampling.LANCZOS).save(tray, "PNG", optimize=True)
    (ICONS / "tray.png").write_bytes(tray.getvalue())
    write_ico(ICONS / "icon.ico", [16, 20, 24, 32, 40, 48, 64, 128, 256])
    print(f"wrote {len(outputs) + 2} icons to {ICONS}")


if __name__ == "__main__":
    main()
