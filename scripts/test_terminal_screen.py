import tempfile
import unittest
from pathlib import Path
import sqlite3

from terminal_screen import Screen
from diagnostics import fixture, summary


class ScreenTests(unittest.TestCase):
    def test_current_screen_excludes_erased_and_alternate_text(self):
        screen = Screen(40, 5)
        screen.feed(b"primary\x1b[?1049h\x1b[2J\x1b[1;1Hunknown\x1b[1;1Htotal: 100000\x1b[K")
        self.assertNotIn("unknown", screen.text)
        self.assertIn("total: 100000", screen.text)
        screen.feed(b"\x1b[?1049l")
        self.assertIn("primary", screen.text)
        self.assertNotIn("100000", screen.text)

    def test_cursor_erase_styling_and_fragmented_queries(self):
        data = b"\x1b[2J\x1b[Habcde\x1b[2DXY\x1b[2;1Hstale\x1b[2K\x1b[3;1H\x1b[31mready\x1b[0m\x1b]0;title\x07\x1b[6n"
        for step in (1, 2, 3, 7, 4096):
            screen = Screen(40, 5)
            for offset in range(0, len(data), step):
                screen.feed(data[offset:offset + step])
            self.assertTrue(screen.text.startswith("abcXY"))
            self.assertIn("ready", screen.text)
            self.assertNotIn("stale", screen.text)
            self.assertNotIn("title", screen.text)
            self.assertEqual(screen.queries, 1)

    def test_fragmented_captured_output_matches_whole_decode(self):
        for path in Path(__file__).with_name("fixtures").glob("*.vt"):
            data = path.read_bytes()
            whole = Screen()
            whole.feed(data)
            self.assertIn("value-", whole.text, str(path))
            for step in (1, 2, 13, 127):
                fragmented = Screen()
                for offset in range(0, len(data), step):
                    fragmented.feed(data[offset:offset + step])
                self.assertEqual(fragmented.text, whole.text, (path, step))
                self.assertEqual(fragmented.pending, whole.pending)

    def test_fixtures_and_diagnostic_percentiles(self):
        with tempfile.TemporaryDirectory() as directory:
            for shape in ("narrow", "wide", "skewed"):
                path = Path(directory) / (shape + ".sqlite")
                fixture(path, 100, shape)
                with sqlite3.connect(path) as db:
                    self.assertEqual(db.execute("SELECT count(*) FROM data").fetchone()[0], 100)
                    self.assertEqual(len(db.execute("PRAGMA table_info(data)").fetchall()), 16 if shape == "wide" else 2)
                    if shape == "skewed":
                        self.assertEqual(db.execute("SELECT count(*) FROM data WHERE value='repeated'").fetchone()[0], 90)
                        self.assertEqual(db.execute("SELECT count(*) FROM data WHERE value IS NULL").fetchone()[0], 1)
                db.close()
        self.assertEqual(summary([dict(open_ms=i) for i in range(1, 21)])["open_ms"]["p95"], 19)
        self.assertIn("unassessed", summary([dict(open_ms=1)])["open_ms"]["p95"])
