"""Draws the Wercode mark with Pillow and writes the Windows .ico and a PNG.

The shape matches assets/brand/logo.svg; keep the two in step by hand.

    python tools/make_icon.py
"""

from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024
SCALE = SIZE / 64
BRAND = Path(__file__).resolve().parent.parent / "assets" / "brand"


def scaled(*values: float) -> list[float]:
    return [v * SCALE for v in values]


def gradient() -> Image.Image:
    """Blue in the top-left corner to teal in the bottom-right."""
    start, end = (0x5A, 0xA2, 0xFF), (0x4F, 0xD1, 0xC5)
    ramp = Image.new("RGB", (256, 1))
    for x in range(256):
        t = x / 255
        ramp.putpixel((x, 0), tuple(round(a + (b - a) * t) for a, b in zip(start, end)))
    return ramp.resize((SIZE * 2, SIZE * 2)).rotate(-35, resample=Image.BICUBIC).crop(
        (SIZE // 2, SIZE // 2, SIZE // 2 + SIZE, SIZE // 2 + SIZE)
    )


def mark() -> Image.Image:
    image = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.rounded_rectangle(
        scaled(2, 2, 62, 62), radius=14 * SCALE, fill="#0e1014", outline="#2a2f3a", width=round(1.5 * SCALE)
    )

    stroke = Image.new("L", (SIZE, SIZE), 0)
    pen = ImageDraw.Draw(stroke)
    points = [(14, 21), (23, 43), (32, 27), (41, 43), (50, 21)]
    width = 6 * SCALE
    pen.line([(x * SCALE, y * SCALE) for x, y in points], fill=255, width=round(width), joint="curve")
    for x, y in points:
        pen.ellipse(
            [x * SCALE - width / 2, y * SCALE - width / 2, x * SCALE + width / 2, y * SCALE + width / 2], fill=255
        )
    image.paste(gradient(), (0, 0), stroke)
    return image


def main() -> None:
    BRAND.mkdir(parents=True, exist_ok=True)
    image = mark()
    image.resize((256, 256), Image.LANCZOS).save(BRAND / "wercode.png")
    image.save(BRAND / "wercode.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    print("wrote", BRAND / "wercode.ico")


if __name__ == "__main__":
    main()
