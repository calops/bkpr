"""Decorative presentation of an unchanged, opaque application screenshot."""

from collections import Counter
import hashlib
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


RIM = 12
RADIUS = 12
BEVEL = 2
MARGIN = 48
SHADOW_SIGMA = 10
SHADOW_OFFSET = (0, 7)
SHADOW_OPACITY = 115
MASK_SCALE = 3


def _linear(channel: int) -> float:
    value = channel / 255
    return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4


def _mix(left: tuple[int, ...], right: tuple[int, ...], weight: float) -> tuple[int, ...]:
    result = []
    for a, b in zip(left, right, strict=True):
        value = _linear(a) * (1 - weight) + _linear(b) * weight
        encoded = value * 12.92 if value <= 0.0031308 else 1.055 * value ** (1 / 2.4) - 0.055
        result.append(round(encoded * 255))
    return tuple(result)


def _rounded_mask(size: tuple[int, int], box: tuple[int, int, int, int], radius: int) -> Image.Image:
    mask = Image.new("L", (size[0] * MASK_SCALE, size[1] * MASK_SCALE))
    left, top, right, bottom = box
    ImageDraw.Draw(mask).rounded_rectangle(
        (left * MASK_SCALE, top * MASK_SCALE, right * MASK_SCALE - 1, bottom * MASK_SCALE - 1),
        radius=radius * MASK_SCALE,
        fill=255,
    )
    return mask.resize(size, Image.Resampling.LANCZOS)


def physicalize(raw: Path, output: Path) -> dict:
    """Save a transparent raised-rim PNG, preserving every opaque source pixel.

    Transparent source pixels and overwriting the evidence image are rejected.
    Rectangle metadata uses an origin and dimensions, never inclusive endpoints.
    """
    if raw.resolve() == output.resolve():
        raise ValueError("The composed output must not overwrite the raw screenshot")
    with Image.open(raw) as source:
        app = source.convert("RGBA")
    if app.getchannel("A").getextrema() != (255, 255):
        raise ValueError("The raw screenshot must be entirely opaque")
    width, height = app.size
    perimeter = Counter()
    for box in ((0, 0, width, 1), (0, height - 1, width, height), (0, 0, 1, height), (width - 1, 0, width, height)):
        perimeter.update(app.crop(box).convert("RGB").get_flattened_data())
    background = perimeter.most_common(1)[0][0]
    foreground = (224, 224, 224)
    # Match appearance.rs's linear-light raised lift and directional lip strengths.
    surface = _mix(background, foreground, 0.06)
    highlight = _mix(surface, foreground, 0.24)
    shade = _mix(surface, (0, 0, 0), 0.24)
    palette = {
        "background": list(background),
        "foreground_reference": list(foreground),
        "surface": list(surface),
        "top_left": list(highlight),
        "bottom_right": list(shade),
        "shadow": [0, 0, 0],
    }
    frame_width, frame_height = width + RIM * 2, height + RIM * 2
    size = (frame_width + MARGIN * 2, frame_height + MARGIN * 2)
    outer = (MARGIN, MARGIN, MARGIN + frame_width, MARGIN + frame_height)
    outer_mask = _rounded_mask(size, outer, RADIUS)
    shadow_box = tuple(value + SHADOW_OFFSET[index % 2] for index, value in enumerate(outer))
    shadow_mask = _rounded_mask(size, shadow_box, RADIUS).filter(ImageFilter.GaussianBlur(SHADOW_SIGMA))
    shadow_mask = shadow_mask.point(lambda alpha: round(alpha * SHADOW_OPACITY / 255))
    composed = Image.new("RGBA", size, (0, 0, 0, 0))
    composed.paste((0, 0, 0, 255), (0, 0, *size), shadow_mask)

    material = Image.new("RGBA", size, (*highlight, 255))
    left, top, right, bottom = outer
    ImageDraw.Draw(material).polygon(
        [(left, bottom), (right, top), (right, bottom)], fill=(*shade, 255)
    )
    inner = (left + BEVEL, top + BEVEL, right - BEVEL, bottom - BEVEL)
    inner_mask = _rounded_mask(size, inner, RADIUS - BEVEL)
    material.paste((*surface, 255), (0, 0, *size), inner_mask)
    material.putalpha(outer_mask)
    composed = Image.alpha_composite(composed, material)
    app_x, app_y = MARGIN + RIM, MARGIN + RIM
    ImageDraw.Draw(composed).rectangle(
        (app_x - 1, app_y - 1, app_x + width, app_y + height), fill=(*shade, 255)
    )
    # No mask, filtering, or resampling may touch the captured application pixels.
    composed.paste(app, (app_x, app_y))
    output.parent.mkdir(parents=True, exist_ok=True)
    composed.save(output, format="PNG")
    return {
        "raw": {"path": str(raw), "width": width, "height": height, "sha256": hashlib.sha256(raw.read_bytes()).hexdigest()},
        "composed": {"path": str(output), "width": size[0], "height": size[1], "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "mode": "RGBA"},
        "app_rect": {"x": app_x, "y": app_y, "width": width, "height": height},
        "frame_rect": {"x": left, "y": top, "width": frame_width, "height": frame_height},
        "palette": palette,
        "bevel": {"rim_px": RIM, "outer_radius_px": RADIUS, "facet_px": BEVEL, "raised_lift": 0.06, "highlight_strength": 0.24, "shade_strength": 0.24, "mask_supersampling": MASK_SCALE},
        "shadow": {"sigma_px": SHADOW_SIGMA, "offset_px": list(SHADOW_OFFSET), "opacity": SHADOW_OPACITY / 255, "margin_px": MARGIN},
        "presentation": "Decorative raised rim; not a native operating-system window",
        "app_pixels_unchanged": True,
    }
