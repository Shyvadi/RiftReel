"""Draws RiftReel's icon set into src-tauri/icons (original artwork).

The mark: a bold black R on a rounded tile that runs from red to dark red, the same at
every size, the tray icon (tray.png) included. It is constructed, not drawn by hand: a
stem, a bowl between two concentric half circles, and a straight leg.

The mark is built here as SVG paths (one per shape, each filled even-odd), which this
script also writes, with its colours, to src-tauri/icons/source.svg and to
ui/src/lib/mark.js for the window's logo, so all three match. The PNG and ICO sizes are
drawn from those paths at 4x and scaled down, so edges stay clean.

usage: python tools/make-icons.py   (needs Pillow)
"""
import io
import math
import re
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps

ROOT = Path(__file__).resolve().parent.parent
ICONS = ROOT / "src-tauri" / "icons"
UI_MARK = ROOT / "ui" / "src" / "lib" / "mark.js"
SIZE = 1024
SUPER = 4
# The tile runs from TILE_TOP (top left) to TILE_BOTTOM (bottom right); the R is INK,
# with a soft SHADOW under it.
TILE_TOP, TILE_BOTTOM = (239, 68, 68), (127, 29, 29)
INK = (16, 16, 18)
SHADOW, SHADOW_OPACITY = (60, 0, 0), 0.3

# The R, centred on the 1024 grid: its height, width and stroke, the bowl's height, the
# leg's width, how far the leg reaches past the bowl, and the gap between the stem and
# the top of the leg.
CENTRE = (512, 512)
R_HEIGHT, R_WIDTH, R_STROKE, R_BOWL = 500, 380, 104, 290
R_LEG, R_REACH, R_NOTCH = 136, 36, 38


def num(value):
    return f"{value:.1f}".rstrip("0").rstrip(".")


def hex_colour(rgb):
    return "#{:02x}{:02x}{:02x}".format(*rgb)


def xy(point):
    return f"{num(point[0])} {num(point[1])}"


def polar(centre, radius, degrees):
    a = math.radians(degrees)
    return centre[0] + radius * math.cos(a), centre[1] + radius * math.sin(a)


def arc(centre, radius, start, end):
    """Cubic curves along a circle from angle `start` to `end` (degrees, either way)."""
    count = max(1, math.ceil(abs(end - start) / 90 - 1e-9))
    step = (end - start) / count
    curves = []
    for i in range(count):
        a, b = math.radians(start + i * step), math.radians(start + (i + 1) * step)
        k = 4 / 3 * math.tan((b - a) / 4) * radius
        p0, p3 = polar(centre, radius, math.degrees(a)), polar(centre, radius, math.degrees(b))
        p1 = (p0[0] - k * math.sin(a), p0[1] + k * math.cos(a))
        p2 = (p3[0] + k * math.sin(b), p3[1] - k * math.cos(b))
        curves.append(f"C{xy(p1)} {xy(p2)} {xy(p3)}")
    return " ".join(curves)


def polygon(points):
    return "M" + " L".join(xy(p) for p in points) + " Z"


def mark():
    """The R's paths: its stem, its bowl (with the counter cut out) and its leg."""
    left, top = CENTRE[0] - R_WIDTH / 2, CENTRE[1] - R_HEIGHT / 2
    right, bottom = left + R_WIDTH, top + R_HEIGHT
    stem = left + R_STROKE
    radius = R_BOWL / 2
    bowl = (right - R_REACH - radius, top + radius)
    bowl_path = (
        f"M{xy((stem - 2, top))} L{xy((bowl[0], top))} {arc(bowl, radius, -90, 90)} "
        f"L{xy((stem - 2, top + R_BOWL))} Z "
        f"M{xy((stem, top + R_STROKE))} L{xy((bowl[0], top + R_STROKE))} "
        f"{arc(bowl, radius - R_STROKE, -90, 90)} L{xy((stem, top + R_BOWL - R_STROKE))} Z"
    )
    leg_top, leg_left = top + R_BOWL - R_STROKE / 2, stem + R_NOTCH
    return [
        polygon([(left, top), (stem, top), (stem, bottom), (left, bottom)]),
        bowl_path,
        polygon([(leg_left, leg_top), (leg_left + R_LEG, leg_top), (right, bottom), (right - R_LEG, bottom)]),
    ]


def subpaths(path, scale):
    """Each subpath of absolute M, L, C and Z commands as points, curves sampled."""
    shapes = []
    for command, args in re.findall(r"([MLCZ])([^MLCZ]*)", path):
        values = [float(v) * scale for v in args.split()]
        if command == "M":
            shapes.append([])
        if command in "ML":
            shapes[-1] += zip(values[0::2], values[1::2])
        elif command == "C":
            for i in range(0, len(values), 6):
                (x0, y0), (x1, y1, x2, y2, x3, y3) = shapes[-1][-1], values[i : i + 6]
                for step in range(1, 33):
                    t = step / 32
                    a, b, c, d = (1 - t) ** 3, 3 * (1 - t) ** 2 * t, 3 * (1 - t) * t * t, t**3
                    shapes[-1].append((a * x0 + b * x1 + c * x2 + d * x3, a * y0 + b * y1 + c * y2 + d * y3))
    return shapes


def draw(paths, scale):
    """The paths filled white (each even-odd, together their union), as an alpha mask."""
    n = SIZE * scale
    mask = Image.new("L", (n, n), 0)
    for path in paths:
        shape = Image.new("L", (n, n), 0)
        for points in subpaths(path, scale):
            layer = Image.new("L", (n, n), 0)
            ImageDraw.Draw(layer).polygon(points, fill=255)
            shape = ImageChops.difference(shape, layer)
        mask = ImageChops.lighter(mask, shape)
    return mask


def tile(scale):
    """The rounded tile, as RGBA at SIZE * scale."""
    n = SIZE * scale
    down = Image.linear_gradient("L").resize((n, n), Image.Resampling.BILINEAR)
    diagonal = ImageChops.add(down, down.transpose(Image.Transpose.TRANSPOSE), scale=2)
    image = ImageOps.colorize(diagonal, black=TILE_TOP, white=TILE_BOTTOM).convert("RGBA")
    mask = Image.new("L", (n, n), 0)
    margin = 48 * scale
    ImageDraw.Draw(mask).rounded_rectangle(
        [margin, margin, n - margin, n - margin], radius=208 * scale, fill=255
    )
    image.putalpha(mask)
    return image


def compose(glyph):
    """The tile with the glyph in INK and its soft shadow, at SIZE * SUPER."""
    base = tile(SUPER)
    shadow = Image.new("RGBA", base.size, SHADOW + (0,))
    shadow.putalpha(
        glyph.point(lambda v: int(v * SHADOW_OPACITY)).filter(ImageFilter.GaussianBlur(14 * SUPER))
    )
    shadow = ImageChops.offset(shadow, 0, 12 * SUPER)
    tile_alpha = base.getchannel("A")
    shadow.putalpha(ImageChops.multiply(shadow.getchannel("A"), tile_alpha))
    base = Image.alpha_composite(base, shadow)
    ink = Image.new("RGBA", base.size, INK + (0,))
    ink.putalpha(glyph)
    return Image.alpha_composite(base, ink)


MARK = mark()
FULL = compose(draw(MARK, SUPER))


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


def write_svg(path):
    paths = "\n".join(f'    <path d="{d}"/>' for d in MARK)
    path.write_text(
        f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
  <!-- RiftReel's mark (original artwork), written by tools/make-icons.py: edit it there. -->
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{hex_colour(TILE_TOP)}"/>
      <stop offset="1" stop-color="{hex_colour(TILE_BOTTOM)}"/>
    </linearGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="0" dy="12" stdDeviation="14" flood-color="{hex_colour(SHADOW)}" flood-opacity="{SHADOW_OPACITY}"/>
    </filter>
  </defs>
  <rect x="48" y="48" width="928" height="928" rx="208" fill="url(#tile)"/>
  <g fill="{hex_colour(INK)}" fill-rule="evenodd" filter="url(#shadow)">
{paths}
  </g>
</svg>
""",
        newline="\n",
    )


def write_ui_mark(path):
    paths = "\n".join(f"  '{d}'," for d in MARK)
    path.write_text(
        "// RiftReel's mark on the 1024 grid: paths in INK, each filled even-odd, on a tile\n"
        "// running from TILE[0] (top left) to TILE[1]. Written by tools/make-icons.py: edit\n"
        "// it there.\n"
        f"export const TILE = ['{hex_colour(TILE_TOP)}', '{hex_colour(TILE_BOTTOM)}'];\n"
        f"export const INK = '{hex_colour(INK)}';\n"
        f"export const MARK = [\n{paths}\n];\n",
        newline="\n",
    )


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
    write_svg(ICONS / "source.svg")
    write_ui_mark(UI_MARK)
    print(f"wrote {len(outputs) + 2} icons to {ICONS}, and {UI_MARK.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
