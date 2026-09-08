"""Regression for the pinned terminal emulator's orphaned wide-cell stub."""
import unittest
from unittest.mock import patch

import os
import pathlib

import pyte

from test_lvu_pty import PtyApp, isolated_launch


class ScreenTextTests(unittest.TestCase):
    def test_launch_isolates_ambient_no_color_but_preserves_explicit_override(self):
        with patch.dict(os.environ, {"NO_COLOR": "1"}):
            for arguments in (["--demo"], ["fixture.log"]):
                with self.subTest(arguments=arguments):
                    _, environment = isolated_launch(arguments, pathlib.Path("/fixture"), None)
                    self.assertEqual(environment["NO_COLOR"], "")
                    _, explicit = isolated_launch(
                        arguments, pathlib.Path("/fixture"), {"NO_COLOR": "1"}
                    )
                    self.assertEqual(explicit["NO_COLOR"], "1")
            self.assertEqual(os.environ["NO_COLOR"], "1")

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
