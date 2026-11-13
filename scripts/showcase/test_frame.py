"""Synthetic pixels test framing only, never native screenshot acceptance."""

import hashlib
import json
from math import cos, hypot, pi, sin
from pathlib import Path
import tempfile
import unittest

from PIL import Image

from scripts.showcase.frame import physicalize


class PhysicalizeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.raw = self.root / "raw.png"
        self.output = self.root / "presentation" / "framed.png"

    def source(self, size=(73, 41), mode="RGBA"):
        image = Image.new("RGBA", size)
        image.putdata([
            ((x * 17 + y * 3) % 256, (y * 29 + x) % 256, (x * 7 + y * 11) % 256, 255)
            for y in range(size[1])
            for x in range(size[0])
        ])
        image.convert(mode).save(self.raw)
        return image

    def test_every_application_pixel_and_raw_evidence_remain_unchanged(self):
        expected = self.source()
        raw_bytes = self.raw.read_bytes()
        metadata = physicalize(self.raw, self.output)
        rect = metadata["app_rect"]
        with Image.open(self.output) as composed:
            crop = composed.crop((rect["x"], rect["y"], rect["x"] + rect["width"], rect["y"] + rect["height"]))
            self.assertEqual(crop.size, expected.size)
            self.assertEqual(crop.tobytes(), expected.tobytes())
        self.assertEqual(self.raw.read_bytes(), raw_bytes)
        self.assertEqual(metadata["raw"]["sha256"], hashlib.sha256(raw_bytes).hexdigest())
        self.assertEqual(metadata["composed"]["sha256"], hashlib.sha256(self.output.read_bytes()).hexdigest())
        json.dumps(metadata)

    def test_transparent_exterior_soft_shadow_and_opaque_inset(self):
        self.source()
        metadata = physicalize(self.raw, self.output)
        with Image.open(self.output) as composed:
            self.assertEqual(composed.mode, "RGBA")
            alpha = composed.getchannel("A")
            self.assertEqual(alpha.getextrema(), (0, 255))
            width, height = composed.size
            for point in ((0, 0), (width - 1, 0), (0, height - 1), (width - 1, height - 1)):
                self.assertEqual(alpha.getpixel(point), 0)
            frame = metadata["frame_rect"]
            shadow_strip = alpha.crop((frame["x"] + frame["width"], 0, width, height))
            self.assertGreater(shadow_strip.getextrema()[1], 0)
            self.assertLess(shadow_strip.getextrema()[1], 255)
            app = metadata["app_rect"]
            for point in ((app["x"], app["y"]), (app["x"] + app["width"] - 1, app["y"] + app["height"] - 1)):
                self.assertEqual(alpha.getpixel(point), 255)

    def test_geometry_keeps_square_content_clear_of_rounded_exterior(self):
        for size in ((1, 1), (2, 103), (107, 2), (73, 41)):
            with self.subTest(size=size):
                expected = self.source(size, mode="RGB")
                metadata = physicalize(self.raw, self.output)
                app, frame = metadata["app_rect"], metadata["frame_rect"]
                radius = metadata["bevel"]["outer_radius_px"]
                insets = (
                    app["x"] - frame["x"], app["y"] - frame["y"],
                    frame["x"] + frame["width"] - app["x"] - app["width"],
                    frame["y"] + frame["height"] - app["y"] - app["height"],
                )
                self.assertEqual(len(set(insets)), 1)
                self.assertLess(max(insets), 12)
                self.assertLess(hypot(max(0, radius - insets[0]), max(0, radius - insets[1])), radius)
                self.assertEqual((app["width"], app["height"]), size)
                self.assertEqual((metadata["raw"]["width"], metadata["raw"]["height"]), size)
                with Image.open(self.output) as composed:
                    self.assertEqual(composed.size, (metadata["composed"]["width"], metadata["composed"]["height"]))
                    self.assertGreater(composed.width, frame["x"] + frame["width"])
                    self.assertGreater(composed.height, frame["y"] + frame["height"])
                    self.assertEqual(composed.crop((app["x"], app["y"], app["x"] + size[0], app["y"] + size[1])).tobytes(), expected.tobytes())

    def test_rim_matches_sidebar_instead_of_foreground_or_other_panes(self):
        sidebar = (37, 42, 55, 255)
        image = Image.new("RGBA", (160, 100), (245, 220, 200, 255))
        image.paste(sidebar, (0, 0, 40, 100))
        for y in range(25, 75, 10):
            image.paste((224, 224, 224, 255), (2, y, 18, y + 2))
        image.save(self.raw)
        metadata = physicalize(self.raw, self.output)
        self.assertEqual(metadata["palette"]["background"], list(sidebar[:3]))
        self.assertEqual(metadata["palette"]["surface"], list(sidebar[:3]))
        app = metadata["app_rect"]
        with Image.open(self.output) as composed:
            for point in (
                (app["x"] + 80, app["y"] - 1),
                (app["x"] + 80, app["y"] + 100),
                (app["x"] - 1, app["y"] + 50),
                (app["x"] + 160, app["y"] + 50),
            ):
                self.assertEqual(composed.getpixel(point), sidebar)

    def test_directional_bevel_changes_gradually_around_rounded_corner(self):
        surface = (40, 48, 64, 255)
        Image.new("RGBA", (160, 100), surface).save(self.raw)
        metadata = physicalize(self.raw, self.output)
        frame = metadata["frame_rect"]
        right, top = frame["x"] + frame["width"], frame["y"]
        radius = metadata["bevel"]["outer_radius_px"]
        center_x, center_y = right - radius, top + radius
        offsets = tuple(
            (round((radius - 0.5) * sin(step * pi / 64)),
             -round((radius - 0.5) * cos(step * pi / 64)))
            for step in range(33)
        )
        with Image.open(self.output) as composed:
            colors = [composed.getpixel((center_x + x, center_y + y))[:3] for x, y in offsets]
            self.assertGreater(sum(colors[0]), sum(colors[-1]))
            self.assertGreaterEqual(len(set(colors)), 4)
            top_strip = [composed.getpixel((x, top))[:3] for x in range(frame["x"] + radius, right - radius)]
            for channel in zip(*top_strip, strict=True):
                self.assertLessEqual(max(channel) - min(channel), 1)
            right_strip = [composed.getpixel((right - 1, y))[:3] for y in range(top + radius, top + frame["height"] - radius)]
            for channel in zip(*right_strip, strict=True):
                self.assertLessEqual(max(channel) - min(channel), 1)

    def test_rounded_edge_has_partial_coverage_not_stair_steps(self):
        Image.new("RGBA", (160, 100), (40, 48, 64, 255)).save(self.raw)
        metadata = physicalize(self.raw, self.output)
        frame = metadata["frame_rect"]
        with Image.open(self.output) as composed:
            radius = metadata["bevel"]["outer_radius_px"]
            corner = composed.crop((frame["x"], frame["y"], frame["x"] + radius, frame["y"] + radius))
            edge_alphas = {alpha for red, green, blue, alpha in corner.get_flattened_data()
                           if red + green + blue > 0 and 115 < alpha < 255}
            self.assertGreaterEqual(len(edge_alphas), 3)
            self.assertLess(composed.getpixel((frame["x"], frame["y"]))[3], 115)

    def test_missing_raw_reports_its_path_without_creating_output(self):
        with self.assertRaises(FileNotFoundError) as caught:
            physicalize(self.raw, self.output)
        self.assertEqual(Path(caught.exception.filename), self.raw)
        self.assertFalse(self.output.exists())

    def test_transparent_source_is_rejected_instead_of_changing_pixels(self):
        image = self.source()
        image.putpixel((0, 0), (31, 47, 59, 128))
        image.save(self.raw)
        with self.assertRaisesRegex(ValueError, "entirely opaque"):
            physicalize(self.raw, self.output)
        self.assertFalse(self.output.exists())

    def test_raw_cannot_be_overwritten(self):
        self.source()
        raw_bytes = self.raw.read_bytes()
        with self.assertRaisesRegex(ValueError, "must not overwrite"):
            physicalize(self.raw, self.raw)
        self.assertEqual(self.raw.read_bytes(), raw_bytes)


if __name__ == "__main__":
    unittest.main()
