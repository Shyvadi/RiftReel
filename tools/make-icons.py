"""Draws RiftReel's icon set into src-tauri/icons (original artwork).

The mark: a white dragon coiled into a reel around the letter R, biting the tip of its
own tail, on a rounded tile that runs from teal to blue. The same mark is used at every
size, the tray icon (tray.png) included.

It is constructed, not drawn by hand. The body lies between two circles about one
centre. The tail tapers to its tip between two more circles, each tangent to one of
those, so the taper flows out of the body. The head is the body bulged out to a slightly
larger circle, cut square at the nose and chin around a narrow mouth. The horn is a
crescent of two arcs and the eye is a lens of two arcs. The R is a stem, a bowl between
two concentric half circles, and a leg. Every other edge is straight.

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

# On the 1024 grid; angles in degrees, clockwise from 3 o'clock. The body runs clockwise
# from the tail's tip (TIP) round to the head, and is whole from TIP + 180 on.
CENTRE = (530, 548)
OUTER, INNER = 300, 180
MIDDLE = (OUTER + INNER) / 2
TIP, NECK = 293, 244
# The head: how far it bulges past the body, where the nose and chin are cut, the corner
# of the mouth, and how far each lip stands from the middle of the body.
BULGE, NOSE, CHIN, MOUTH, LIP = 22, 303, 299, 286, 14
# The horn: base from and to (angles), the tip's angle and height above the head, and
# the radii of its front and back arcs.
HORN = (256, 276, 232, 132, 230, 180)
# The eye: its angle and radius, length, height and tilt.
EYE = (284, 262, 48, 15, 3)
# The R in the middle: height, width, stroke, the bowl's height, and the leg's width.
R_HEIGHT, R_WIDTH, R_STROKE, R_BOWL, R_LEG = 236, 176, 46, 140, 58


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


def arc_between(p0, p1, radius, away_from):
    """The shorter arc of the given radius from p0 to p1, bulging away from `away_from`."""
    half = math.dist(p0, p1) / 2
    middle = ((p0[0] + p1[0]) / 2, (p0[1] + p1[1]) / 2)
    normal = (-(p1[1] - p0[1]) / (2 * half), (p1[0] - p0[0]) / (2 * half))
    reach = math.sqrt(max(radius * radius - half * half, 0))
    best = None
    for side in (1, -1):
        centre = (middle[0] + side * reach * normal[0], middle[1] + side * reach * normal[1])
        start = math.degrees(math.atan2(p0[1] - centre[1], p0[0] - centre[0]))
        end = math.degrees(math.atan2(p1[1] - centre[1], p1[0] - centre[0]))
        sweep = (end - start + 180) % 360 - 180
        bulge = math.dist(polar(centre, radius, start + sweep / 2), away_from)
        if best is None or bulge > best[0]:
            best = (bulge, centre, start, start + sweep)
    _, centre, start, end = best
    return arc(centre, radius, start, end)


def body():
    """The whole part of the body, from where the tail's taper ends round to the neck."""
    start, end = TIP + 180 - 3, NECK + 360
    return (
        f"M{xy(polar(CENTRE, OUTER, start))} {arc(CENTRE, OUTER, start, end)} "
        f"L{xy(polar(CENTRE, INNER, end))} {arc(CENTRE, INNER, end, start)} Z"
    )


def tail():
    """The tail, tapering to its tip between two circles: one through the tip and tangent
    to the outer circle, one through the tip and tangent to the inner circle, both
    where the taper ends."""
    whole = TIP + 180
    toward = polar((0, 0), 1, whole)
    shift_out, shift_in = (OUTER - MIDDLE) / 2, (MIDDLE - INNER) / 2
    outer = (CENTRE[0] + shift_out * toward[0], CENTRE[1] + shift_out * toward[1])
    inner = (CENTRE[0] - shift_in * toward[0], CENTRE[1] - shift_in * toward[1])
    outer_r, inner_r = (OUTER + MIDDLE) / 2, (MIDDLE + INNER) / 2
    return (
        f"M{xy(polar(outer, outer_r, TIP))} {arc(outer, outer_r, TIP, whole)} "
        f"L{xy(polar(inner, inner_r, whole))} {arc(inner, inner_r, whole, TIP)} Z"
    )


def head():
    """The head, with its eye cut out."""
    rim = OUTER + BULGE
    outline = (
        f"M{xy(polar(CENTRE, rim, NECK - 3))} {arc(CENTRE, rim, NECK - 3, NOSE)} "
        f"L{xy(polar(CENTRE, MIDDLE + LIP, NOSE))} L{xy(polar(CENTRE, MIDDLE, MOUTH))} "
        f"L{xy(polar(CENTRE, MIDDLE - LIP, CHIN))} L{xy(polar(CENTRE, INNER, CHIN))} "
        f"{arc(CENTRE, INNER, CHIN, NECK - 3)} Z"
    )
    at, radius, length, height, tilt = EYE
    middle = polar(CENTRE, radius, at)
    ends = [polar(middle, length / 2, at + 90 + tilt + side) for side in (180, 0)]
    up, down = polar(middle, 1000, at + tilt), polar(middle, 1000, at + tilt + 180)
    lens = (length * length / 4 + height * height / 4) / height
    eye = (
        f"M{xy(ends[0])} {arc_between(ends[0], ends[1], lens, down)} "
        f"{arc_between(ends[1], ends[0], lens, up)} Z"
    )
    return outline + " " + eye


def horn(start, end, tip_at, height, front, back):
    """A crescent from the head's rim, swept back: two arcs bulging outward."""
    rim = OUTER + BULGE - 8
    base_back, base_front = polar(CENTRE, rim, start), polar(CENTRE, rim, end)
    point = polar(CENTRE, OUTER + BULGE + height, tip_at)
    return (
        f"M{xy(base_front)} {arc_between(base_front, point, front, CENTRE)} "
        f"{arc_between(point, base_back, back, CENTRE)} Z"
    )


def letter_r():
    """The R, centred in the coil: its stem, its bowl (with the counter cut out) and its
    leg, which reaches a little past the bowl."""
    left, top = CENTRE[0] - R_WIDTH / 2, CENTRE[1] - R_HEIGHT / 2
    right, bottom = left + R_WIDTH, top + R_HEIGHT
    stem = left + R_STROKE
    radius = R_BOWL / 2
    bowl = (right - 18 - radius, top + radius)
    counter = radius - R_STROKE
    bowl_path = (
        f"M{xy((stem - 2, top))} L{xy((bowl[0], top))} {arc(bowl, radius, -90, 90)} "
        f"L{xy((stem - 2, top + R_BOWL))} Z "
        f"M{xy((stem, top + R_STROKE))} L{xy((bowl[0], top + R_STROKE))} "
        f"{arc(bowl, counter, -90, 90)} L{xy((stem, top + R_BOWL - R_STROKE))} Z"
    )
    leg_top, leg_left = top + R_BOWL - R_STROKE / 2, bowl[0] - R_LEG / 2 - 6
    return [
        polygon([(left, top), (stem, top), (stem, bottom), (left, bottom)]),
        bowl_path,
        polygon([(leg_left, leg_top), (leg_left + R_LEG, leg_top), (right, bottom), (right - R_LEG, bottom)]),
    ]


def mark():
    """The mark's paths, one per shape."""
    return [body(), tail(), head(), horn(*HORN)] + letter_r()


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
