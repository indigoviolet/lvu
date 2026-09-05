"""Regression for the pinned terminal emulator's orphaned wide-cell stub."""
import unittest

import pyte

from test_lvu_pty import PtyApp


class ScreenTextTests(unittest.TestCase):
    def test_wide_character_overwrite_retains_column_positions(self):
        app = object.__new__(PtyApp)
        app.screen = pyte.Screen(6, 1)
        stream = pyte.Stream(app.screen)
        stream.feed("界abc")
        self.assertEqual(app.text(), "界abc ")
        stream.feed("\x1b[1;1Hx")
        self.assertEqual(app.text(), "x abc ")
        stream.feed("\x1b[1;2Hy")
        self.assertEqual(app.text(), "xyabc ")


if __name__ == "__main__":
    unittest.main()
