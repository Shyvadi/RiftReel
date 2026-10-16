"""Draws RiftReel's icon set into src-tauri/icons (original artwork).

The mark: a white dragon coiled into a reel around a play button, spines on its back,
biting at its own arrow-tipped tail, on a rounded tile that runs from teal to blue. The
same mark is used at every size, the tray icon (tray.png) included.

The mark is built here as SVG paths (one per shape, each filled even-odd), which this
script also writes to src-tauri/icons/source.svg and to ui/src/lib/mark.js for the
window's logo, so all three match. The PNG and ICO sizes are drawn from those paths at
4x and scaled down, so edges stay clean.

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
TEAL = (20, 184, 166)
BLUE = (37, 99, 235)

# The coil, on the 1024 grid: its centre, its body's middle radius and half thickness,
# and angles in degrees clockwise from 3 o'clock: where the tail ends (thin), where its
# taper ends, and where the neck meets the head.
CENTRE = (512, 548)
MIDDLE, HALF, TAIL_HALF = 252, 50, 18
TAIL_END, TAPER_TO, NECK = -44, 26, 236
# Spines on its back: (angle, height), growing toward the head.
SPINES = [(96, 26), (124, 34), (152, 42), (180, 50), (207, 54)]
# The head and its horns, facing right with (0, 0) in the middle of the neck. It sits
# on the coil at NECK, turned HEAD_TURN degrees up from the coil's direction there.
HEAD_TURN = 38
HEAD = (
    "M-30 -50 C10 -58 50 -72 92 -78 C132 -78 176 -62 214 -44 L228 -46 L246 -22 "
    "L206 -14 L197 10 L186 -9 L150 2 L222 28 C176 50 96 58 40 54 L-30 50 Z"
)
HORNS = [
    "M98 -76 C82 -132 26 -170 -62 -178 C-6 -150 28 -110 40 -66 Z",
    "M26 -62 C8 -96 -36 -122 -98 -124 C-52 -102 -26 -80 -18 -54 Z",
]
EYE, EYE_RADIUS = (112, -40), 16
# The play button in the middle: its height, and how far along each edge a corner rounds.
PLAY_HEIGHT, PLAY_ROUND = 190, 34


def num(value):
    return f"{value:.1f}".rstrip("0").rstrip(".")


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


def body():
    """The coil from the tail's thin end round to the neck."""

    def half(degrees):
        t = min(1, max(0, (degrees - TAIL_END) / (TAPER_TO - TAIL_END)))
        return TAIL_HALF + (HALF - TAIL_HALF) * t * t * (3 - 2 * t)

    taper = [TAIL_END + (TAPER_TO - TAIL_END) * i / 12 for i in range(13)]
    outer = [polar(CENTRE, MIDDLE + half(a), a) for a in taper]
    inner = [polar(CENTRE, MIDDLE - half(a), a) for a in reversed(taper)]
    return " ".join(
        [f"M{xy(outer[0])}"]
        + [f"L{xy(p)}" for p in outer[1:]]
        + [arc(CENTRE, MIDDLE + HALF, TAPER_TO, NECK)]
        + [f"L{xy(polar(CENTRE, MIDDLE - HALF, NECK))}", arc(CENTRE, MIDDLE - HALF, NECK, TAPER_TO)]
        + [f"L{xy(p)}" for p in inner[1:]]
        + ["Z"]
    )


def spine(degrees, height, width=14, lean=8):
    """A spine on the coil's back, leaning toward the tail."""
    root = MIDDLE + HALF - 14
    return polygon([
        polar(CENTRE, root, degrees - width / 2),
        polar(CENTRE, MIDDLE + HALF + height, degrees - lean),
        polar(CENTRE, root, degrees + width / 2),
    ])


def tail_tip(length=24, barb=44, back=6):
    """The tail's arrowhead, pointing on round the coil into the mouth."""
    return polygon([
        polar(CENTRE, MIDDLE + barb, TAIL_END + back),
        polar(CENTRE, MIDDLE, TAIL_END - length),
        polar(CENTRE, MIDDLE - barb, TAIL_END + back),
        polar(CENTRE, MIDDLE, TAIL_END - 2),
    ])


def on_neck(path):
    """Path data drawn facing right about (0, 0), placed and turned onto the neck."""
    x0, y0 = polar(CENTRE, MIDDLE, NECK)
    turn = math.radians(NECK + 90 + HEAD_TURN)
    c, s = math.cos(turn), math.sin(turn)

    def place(match):
        x, y = float(match.group(1)), float(match.group(2))
        return xy((x0 + x * c - y * s, y0 + x * s + y * c))

    return re.sub(r"(-?\d+(?:\.\d+)?) (-?\d+(?:\.\d+)?)", place, path)


def circle(centre, radius):
    return f"M{xy(polar(centre, radius, 0))} {arc(centre, radius, 0, 360)} Z"


def play():
    """The play button, its corners rounded off PLAY_ROUND along each edge."""
    width = PLAY_HEIGHT * math.sqrt(3) / 2
    left = CENTRE[0] - width / 3
    corners = [
        (left, CENTRE[1] - PLAY_HEIGHT / 2),
        (left + width, CENTRE[1]),
        (left, CENTRE[1] + PLAY_HEIGHT / 2),
    ]

    def toward(a, b):
        d = math.dist(a, b)
        return a[0] + (b[0] - a[0]) * PLAY_ROUND / d, a[1] + (b[1] - a[1]) * PLAY_ROUND / d

    path = []
    for i, corner in enumerate(corners):
        come, go = toward(corner, corners[i - 1]), toward(corner, corners[(i + 1) % 3])
        # A quadratic curve through the corner, written as a cubic.
        c1 = tuple(p + (q - p) * 2 / 3 for p, q in zip(come, corner))
        c2 = tuple(p + (q - p) * 2 / 3 for p, q in zip(go, corner))
        path.append(f"{'M' if i == 0 else 'L'}{xy(come)} C{xy(c1)} {xy(c2)} {xy(go)}")
    return " ".join(path) + " Z"


def mark():
    """The mark's paths, one per shape."""
    eye = on_neck(f"M{EYE[0]} {EYE[1]} Z")[1:-2]
    eye_centre = tuple(float(v) for v in eye.split())
    return (
        [body(), tail_tip()]
        + [spine(at, height) for at, height in SPINES]
        + [on_neck(horn) for horn in HORNS]
        + [on_neck(HEAD) + " " + circle(eye_centre, EYE_RADIUS), play()]
    )


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
      <stop offset="0" stop-color="#14b8a6"/>
      <stop offset="1" stop-color="#2563eb"/>
    </linearGradient>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="0" dy="12" stdDeviation="14" flood-color="#081830" flood-opacity="0.35"/>
    </filter>
  </defs>
  <rect x="48" y="48" width="928" height="928" rx="208" fill="url(#tile)"/>
  <g fill="#fff" fill-rule="evenodd" filter="url(#shadow)">
{paths}
  </g>
</svg>
""",
        newline="\n",
    )


def write_ui_mark(path):
    paths = "\n".join(f"  '{d}'," for d in MARK)
    path.write_text(
        "// RiftReel's mark on the 1024 grid: white paths, each filled even-odd, on the\n"
        "// teal-to-blue tile. Written by tools/make-icons.py: edit it there.\n"
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
