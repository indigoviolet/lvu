#!/usr/bin/env python3
"""Focused tests for platform runtime harness terminal-state verification."""

from __future__ import annotations

import importlib.util
import os
import pathlib
import sys
import unittest
from unittest import mock


HARNESS_PATH = pathlib.Path(__file__).with_name("test_unix_runtime.py")
SPEC = importlib.util.spec_from_file_location("platform_unix_runtime", HARNESS_PATH)
assert SPEC is not None and SPEC.loader is not None
HARNESS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HARNESS
SPEC.loader.exec_module(HARNESS)


class TermiosRestorationTests(unittest.TestCase):
    def app(self):
        app = HARNESS.NativePty.__new__(HARNESS.NativePty)
        app.slave = 41
        app.slave_name = "/dev/ttys-test-owned"
        app.before = [1, 2, 3, 4]
        return app

    def test_reopens_recorded_slave_instead_of_reading_revoked_descriptor(self):
        app = self.app()

        def attributes(fd):
            if fd == app.slave:
                raise OSError(25, "Inappropriate ioctl for device")
            self.assertEqual(fd, 73)
            return app.before.copy()

        with (
            mock.patch.object(HARNESS.os, "open", return_value=73) as open_slave,
            mock.patch.object(HARNESS.termios, "tcgetattr", side_effect=attributes) as get_attributes,
            mock.patch.object(HARNESS.os, "close") as close_slave,
        ):
            app.assert_termios_unchanged()

        open_slave.assert_called_once_with(app.slave_name, os.O_RDWR | os.O_NOCTTY)
        get_attributes.assert_called_once_with(73)
        close_slave.assert_called_once_with(73)

    def test_reopened_slave_still_rejects_changed_termios(self):
        app = self.app()
        with (
            mock.patch.object(HARNESS.os, "open", return_value=73),
            mock.patch.object(HARNESS.termios, "tcgetattr", return_value=[9, 2, 3, 4]),
            mock.patch.object(HARNESS.os, "close") as close_slave,
        ):
            with self.assertRaisesRegex(AssertionError, "PTY termios was not restored"):
                app.assert_termios_unchanged()

        close_slave.assert_called_once_with(73)


if __name__ == "__main__":
    unittest.main()
