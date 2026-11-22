"""Draws the Better Task Manager icon: a rust-coloured activity tile split by a bolt.

    py tools/make_icon.py

writes assets/btm.ico, assets/btm.png and assets/btm-preview.png (git-ignored).
Every size is drawn on its own, so the small ones stay readable.
"""

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

ASSETS = Path(__file__).resolve().parent.parent / "assets"
SIZES = [16, 20, 24, 28, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256]
OVERSAMPLE = 8

TILE = [(0.0, (86, 58, 47)), (1.0, (30, 21, 19))]
AREA = [(0.0, (255, 170, 72)), (0.45, (232, 93, 28)), (1.0, (150, 44, 14))]
BOLT = [(0.0, (255, 255, 250)), (1.0, (255, 218, 128))]

# Activity curve and bolt, in units of the tile.
CURVE = [(0.0, 0.78), (0.12, 0.70), (0.22, 0.74), (0.33, 0.50), (0.43, 0.61), (0.55, 0.55),
         (0.67, 0.36), (0.78, 0.45), (0.90, 0.26), (1.0, 0.31)]
BOLT_SHAPE = [(0.585, 0.10), (0.275, 0.555), (0.470, 0.555), (0.395, 0.905), (0.735, 0.415),
              (0.525, 0.415)]
BOLT_SMALL = [(0.62, 0.06), (0.22, 0.58), (0.45, 0.58), (0.36, 0.96), (0.80, 0.40), (0.55, 0.40)]


def gradient(size, stops):
    strip = Image.new("RGBA", (1, size))
    for y in range(size):
        t = y / max(size - 1, 1)
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t0 <= t <= t1:
                k = (t - t0) / (t1 - t0)
                strip.putpixel((0, y), tuple(round(a + (b - a) * k) for a, b in zip(c0, c1)) + (255,))
                break
    return strip.resize((size, size))


def draw(size):
    s = size * OVERSAMPLE
    small = size <= 24
    pad = 0 if small else s * 0.035
    inner = s - 2 * pad

    def points(shape):
        return [(pad + x * inner, pad + y * inner) for x, y in shape]

    def mask(paint):
        m = Image.new("L", (s, s), 0)
        paint(ImageDraw.Draw(m))
        return m

    tile = mask(lambda d: d.rounded_rectangle((pad, pad, s - pad, s - pad), radius=inner * 0.22, fill=255))
    icon = Image.new("RGBA", (s, s))
    icon.paste(gradient(s, TILE), (0, 0), tile)

    if not small:
        grid = mask(lambda d: [
            (d.line([(pad + inner * k, pad), (pad + inner * k, s - pad)], fill=26, width=max(1, s // 110)),
             d.line([(pad, pad + inner * k), (s - pad, pad + inner * k)], fill=26, width=max(1, s // 110)))
            for k in (0.25, 0.5, 0.75)
        ])
        icon.paste((255, 190, 140, 255), (0, 0), ImageChops.multiply(grid, tile))

    area = mask(lambda d: d.polygon(points(CURVE + [(1.0, 1.0), (0.0, 1.0)]), fill=255))
    icon.paste(gradient(s, AREA), (0, 0), ImageChops.multiply(area, tile))
    if not small:
        edge = mask(lambda d: d.line(points(CURVE), fill=255, width=max(2, s // 45), joint="curve"))
        icon.paste((255, 206, 128, 255), (0, 0), ImageChops.multiply(edge, tile))

    shape = points(BOLT_SMALL if small else BOLT_SHAPE)
    bolt = mask(lambda d: d.polygon(shape, fill=255))
    if not small:
        shadow = bolt.filter(ImageFilter.GaussianBlur(s / 38)).point(lambda v: int(v * 0.55))
        icon.paste((40, 8, 0, 255), (0, 0), ImageChops.multiply(shadow, tile))
    icon.paste(gradient(s, BOLT), (0, 0), bolt)
    return icon.resize((size, size), Image.LANCZOS)


def main():
    ASSETS.mkdir(exist_ok=True)
    frames = [draw(size) for size in SIZES]
    frames[-1].save(ASSETS / "btm.ico", sizes=[(n, n) for n in SIZES], append_images=frames[:-1])
    frames[-1].save(ASSETS / "btm.png")

    shown = [256, 128, 64, 48, 32, 24, 20, 16]
    width = sum(shown) + 18 * (len(shown) + 1)
    preview = Image.new("RGBA", (width, 2 * 292), (32, 32, 32, 255))
    preview.paste((243, 243, 243, 255), (0, 292, width, 584))
    x = 18
    for n in shown:
        frame = frames[SIZES.index(n)]
        preview.alpha_composite(frame, (x, 18))
        preview.alpha_composite(frame, (x, 292 + 18))
        x += n + 18
    preview.save(ASSETS / "btm-preview.png")
    print("icon written")


if __name__ == "__main__":
    main()
