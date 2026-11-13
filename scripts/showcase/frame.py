"""Decorative presentation of an unchanged, opaque application screenshot."""

from collections import Counter
import hashlib
from math import hypot, sqrt
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter


RIM = 5
RADIUS = 15
BEVEL = 1.2
MARGIN = 48
SHADOW_SIGMA = 10
SHADOW_OFFSET = (0, 7)
SHADOW_OPACITY = 115
MASK_SCALE = 4


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


def _rounded_mask(size: tuple[int, int], box: tuple[int, int, int, int], radius: float) -> Image.Image:
    mask = Image.new("L", (size[0] * MASK_SCALE, size[1] * MASK_SCALE))
    left, top, right, bottom = box
    ImageDraw.Draw(mask).rounded_rectangle(
        (left * MASK_SCALE, top * MASK_SCALE, right * MASK_SCALE - 1, bottom * MASK_SCALE - 1),
        radius=radius * MASK_SCALE,
        fill=255,
    )
    return mask.resize(size, Image.Resampling.BOX)


def _bevel_material(
    size: tuple[int, int], box: tuple[int, int, int, int], radius: float,
    surface: tuple[int, ...], highlight: tuple[int, ...], shade: tuple[int, ...],
) -> Image.Image:
    material = Image.new("RGBA", size, (*surface, 255))
    pixels = material.load()
    left, top, right, bottom = box
    center_x, center_y = (left + right) / 2, (top + bottom) / 2
    straight_x = (right - left) / 2 - radius
    straight_y = (bottom - top) / 2 - radius
    diagonal = sqrt(2)
    for y in range(top, bottom):
        spans = ((left, right),) if y < top + RIM or y >= bottom - RIM else (
            (left, left + RIM), (right - RIM, right)
        )
        for start, stop in spans:
            for x in range(start, stop):
                dx, dy = x + 0.5 - center_x, y + 0.5 - center_y
                # A rounded-box normal keeps lighting continuous through corners.
                qx, qy = abs(dx) - straight_x, abs(dy) - straight_y
                nx, ny = max(qx, 0), max(qy, 0)
                length = hypot(nx, ny)
                distance = length + min(max(qx, qy), 0) - radius
                weight = max(0, min(1, 1 + distance / BEVEL))
                if not weight:
                    continue
                if length:
                    nx, ny = nx / length, ny / length
                elif qx > qy:
                    nx, ny = 1, 0
                else:
                    nx, ny = 0, 1
                nx *= 1 if dx >= 0 else -1
                ny *= 1 if dy >= 0 else -1
                light = -(nx + ny) / diagonal
                tint = highlight if light > 0 else shade
                pixels[x, y] = (*_mix(surface, tint, abs(light) * weight), 255)
    return material


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
    # The left eighth stays inside the sidebar, away from pane chrome and its
    # unrelated backgrounds. A modal patch tolerates text and selection rows.
    sample = (0, height // 4, max(1, width // 8), max(height // 4 + 1, height * 3 // 4))
    background = Counter(app.crop(sample).convert("RGB").get_flattened_data()).most_common(1)[0][0]
    foreground = (224, 224, 224)
    surface = background
    highlight = _mix(surface, foreground, 0.24)
    shade = _mix(surface, (0, 0, 0), 0.24)
    palette = {
        "background": list(background),
        "background_sample_rect": {"x": sample[0], "y": sample[1], "width": sample[2] - sample[0], "height": sample[3] - sample[1]},
        "foreground_reference": list(foreground),
        "surface": list(surface),
        "top_left": list(highlight),
        "bottom_right": list(shade),
        "shadow": [0, 0, 0],
    }
    frame_width, frame_height = width + RIM * 2, height + RIM * 2
    radius = min(RADIUS, frame_width / 2, frame_height / 2)
    size = (frame_width + MARGIN * 2, frame_height + MARGIN * 2)
    outer = (MARGIN, MARGIN, MARGIN + frame_width, MARGIN + frame_height)
    outer_mask = _rounded_mask(size, outer, radius)
    shadow_box = tuple(value + SHADOW_OFFSET[index % 2] for index, value in enumerate(outer))
    shadow_mask = _rounded_mask(size, shadow_box, radius).filter(ImageFilter.GaussianBlur(SHADOW_SIGMA))
    shadow_mask = shadow_mask.point(lambda alpha: round(alpha * SHADOW_OPACITY / 255))
    composed = Image.new("RGBA", size, (0, 0, 0, 0))
    composed.paste((0, 0, 0, 255), (0, 0, *size), shadow_mask)

    material = _bevel_material(size, outer, radius, surface, highlight, shade)
    left, top, right, bottom = outer
    material.putalpha(outer_mask)
    composed = Image.alpha_composite(composed, material)
    app_x, app_y = MARGIN + RIM, MARGIN + RIM
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
        "bevel": {"rim_px": RIM, "outer_radius_px": radius, "facet_px": BEVEL, "raised_lift": 0, "highlight_strength": 0.24, "shade_strength": 0.24, "mask_supersampling": MASK_SCALE},
        "shadow": {"sigma_px": SHADOW_SIGMA, "offset_px": list(SHADOW_OFFSET), "opacity": SHADOW_OPACITY / 255, "margin_px": MARGIN},
        "presentation": "Decorative raised rim; not a native operating-system window",
        "app_pixels_unchanged": True,
    }
