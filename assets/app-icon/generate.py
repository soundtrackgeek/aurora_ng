"""Draws the Aurora NG app icon and writes assets/app-icon/aurora_ng.ico.

Run from the repository root:  python assets/app-icon/generate.py
Needs Pillow and numpy. The .ico is embedded into the exe by build.rs.
"""

from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

HERE = Path(__file__).parent
BIG = 1024  # master size; every icon size is scaled down from this

# Colors from the app's palette (src/app.rs, mod color).
SKY_TOP = (6, 8, 18)
SKY_BOTTOM = (24, 32, 58)
TEAL = (94, 234, 212)  # ACCENT
GREEN = (74, 222, 160)
VIOLET = (167, 139, 250)
PINK = (244, 114, 182)  # LOVE
NOTE = (240, 253, 250)


def tile_mask(size, margin, radius):
    """A rounded square, drawn 4x larger and scaled down for smooth edges."""
    s = size * 4
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (margin * 4, margin * 4, s - margin * 4 - 1, s - margin * 4 - 1),
        radius=radius * 4,
        fill=255,
    )
    return mask.resize((size, size), Image.LANCZOS)


def sky(size):
    t = np.linspace(0, 1, size)[:, None, None]
    top, bottom = np.array(SKY_TOP, float), np.array(SKY_BOTTOM, float)
    rgb = top + (bottom - top) * t
    return np.broadcast_to(rgb, (size, size, 3)).copy()


def aurora_band(size, color, base, amp, freq, phase, height, strength, rays):
    """One aurora curtain: bright along a wavy lower edge, fading upward."""
    x = np.arange(size)[None, :] / size
    y = np.arange(size)[:, None] / size
    edge = (base + amp * np.sin(freq * x * 2 * np.pi + phase)
            + amp * 0.35 * np.sin(freq * 2.7 * x * 2 * np.pi + phase * 1.9))
    # The curtain is taller in some places than others.
    tall = height * (0.75 + 0.5 * np.sin(x * 2.3 * np.pi + phase * 0.7) ** 2)
    above = np.clip(edge - y, 0, None)
    below = np.clip(y - edge, 0, None)
    glow = np.where(y <= edge, np.exp(-above / tall), np.exp(-((below / 0.02) ** 2)))
    # Brighter right along the lower edge, like real aurora.
    glow = glow * (1 + 0.6 * np.exp(-((above / 0.03) ** 2)))
    if rays:
        # Uneven vertical streaks make it read as a curtain rather than a smear.
        rng = np.random.default_rng(int(phase * 100))
        streaks = np.zeros_like(x)
        for k in range(4):
            f = rays * rng.uniform(0.5, 1.6)
            streaks = streaks + np.sin(x * f * 2 * np.pi + rng.uniform(0, 6.3)) / 4
        streaks = 0.78 + 0.22 * streaks
    else:
        streaks = 1.0
    # Fade out toward the left and right sides.
    sides = np.sin(np.clip(x, 0, 1) * np.pi) ** 0.5
    intensity = glow * streaks * sides * strength
    return intensity[:, :, None] * np.array(color, float)[None, None, :]


def stars(rgb, size):
    rng = np.random.default_rng(7)
    for _ in range(26):
        x, y = rng.uniform(0.12, 0.88) * size, rng.uniform(0.1, 0.42) * size
        r = rng.uniform(0.003, 0.007) * size
        b = rng.uniform(120, 230)
        yy, xx = np.ogrid[:size, :size]
        d = np.exp(-(((xx - x) ** 2 + (yy - y) ** 2) / (r * r)))
        rgb += d[:, :, None] * b


def notes_layer(size, scale=1.0):
    """Two beamed eighth notes, as a white RGBA layer.

    Drawn 4x larger and scaled down, so the edges are smooth."""
    ss = 4
    n = size * ss
    mask = Image.new("L", (n, n), 0)
    d = ImageDraw.Draw(mask)
    s = n * scale
    cx, cy = n * 0.5, n * 0.6

    def p(x, y):
        return (cx + x * s, cy + y * s)

    def head(x, y):
        # A tilted ellipse as a polygon.
        a, b, tilt = 0.105, 0.075, np.radians(-22)
        t = np.linspace(0, 2 * np.pi, 120)
        ex, ey = a * np.cos(t), b * np.sin(t)
        rx = ex * np.cos(tilt) - ey * np.sin(tilt)
        ry = ex * np.sin(tilt) + ey * np.cos(tilt)
        d.polygon([p(x + u, y + v) for u, v in zip(rx, ry)], fill=255)

    stem_w, beam = 0.042, 0.075
    left, right = (-0.13, 0.17), (0.15, 0.11)  # head centers; right note sits higher
    top_l, top_r = -0.23, -0.29
    for (hx, hy), top in ((left, top_l), (right, top_r)):
        head(hx, hy)
        sx = hx + 0.088  # stem rises from the head's right side
        d.rectangle((*p(sx - stem_w, top), *p(sx, hy - 0.01)), fill=255)
    lx, rx = left[0] + 0.088, right[0] + 0.088
    d.polygon(
        [p(lx - stem_w, top_l), p(rx, top_r), p(rx, top_r + beam), p(lx - stem_w, top_l + beam)],
        fill=255,
    )
    mask = mask.resize((size, size), Image.LANCZOS)
    layer = Image.new("RGBA", (size, size), NOTE + (0,))
    layer.putalpha(mask)
    return layer


def render(size, detail=True):
    margin = round(size * 0.04)
    rgb = sky(size)
    if detail:
        stars(rgb, size)
        bands = [
            (PINK, 0.20, 0.035, 1.1, 4.0, 0.10, 0.40, 23),
            (VIOLET, 0.30, 0.05, 0.9, 1.2, 0.17, 0.70, 19),
            (GREEN, 0.50, 0.06, 0.8, 0.3, 0.20, 0.65, 17),
            (TEAL, 0.43, 0.07, 1.0, 2.2, 0.16, 0.95, 21),
        ]
    else:
        # Small sizes: two broad bands, no stars, so it stays crisp.
        bands = [
            (VIOLET, 0.32, 0.05, 0.9, 1.2, 0.22, 0.9, 0),
            (TEAL, 0.46, 0.06, 1.0, 2.2, 0.22, 1.1, 0),
        ]
    for color, base, amp, freq, phase, height, strength, rays in bands:
        rgb += aurora_band(size, color, base, amp, freq, phase, height, strength, rays) * 0.8
    img = Image.fromarray(np.clip(rgb, 0, 255).astype(np.uint8), "RGB").convert("RGBA")

    notes = notes_layer(size, scale=1.0 if detail else 1.25)
    # Soft teal glow behind the notes so they lift off the aurora.
    glow_alpha = notes.getchannel("A").filter(ImageFilter.GaussianBlur(size * 0.035))
    glow = Image.new("RGBA", (size, size), TEAL + (0,))
    glow.putalpha(glow_alpha.point(lambda a: int(a * 0.7)))
    shadow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    shadow.putalpha(notes.getchannel("A").filter(ImageFilter.GaussianBlur(size * 0.012)).point(lambda a: int(a * 0.6)))
    img = Image.alpha_composite(img, glow)
    img = Image.alpha_composite(img, shadow)
    img = Image.alpha_composite(img, notes)

    # A faint rim light on the tile edge.
    mask = tile_mask(size, margin, round(size * 0.2))
    inner = tile_mask(size, margin + max(1, round(size * 0.004)), round(size * 0.2))
    rim = Image.new("RGBA", (size, size), (255, 255, 255, 0))
    rim.putalpha(Image.fromarray(
        ((np.array(mask, float) - np.array(inner, float)).clip(0) * 0.10).astype(np.uint8)
    ))
    img = Image.alpha_composite(img, rim)
    img.putalpha(mask)
    return img


def main():
    big_detail = render(BIG, detail=True)
    big_simple = render(BIG, detail=False)
    big_detail.save(HERE / "aurora_ng.png")
    sizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]
    images = [
        (big_detail if n >= 40 else big_simple).resize((n, n), Image.LANCZOS)
        for n in sizes
    ]
    images[-1].save(
        HERE / "aurora_ng.ico",
        format="ICO",
        sizes=[(n, n) for n in sizes],
        append_images=images[:-1],
    )
    # A preview strip of every size, for checking by eye.
    strip = Image.new("RGBA", (sum(sizes) + 10 * len(sizes) + 10, 276), (32, 32, 32, 255))
    x = 10
    for n, im in zip(sizes, images):
        strip.alpha_composite(im, (x, 266 - n))
        x += n + 10
    strip.save(HERE / "preview.png")


if __name__ == "__main__":
    main()
