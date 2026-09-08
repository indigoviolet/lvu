#!/usr/bin/env python3
"""Safety tests for the janitor's marked reproducer-tree sweep."""

from __future__ import annotations

import importlib.util
import os
import pathlib
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).with_name("janitor.py")
SPEC = importlib.util.spec_from_file_location("lvu_janitor", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
janitor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(janitor)


class ReproducerSweepTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="janitor-fixtures-")
        self.root = pathlib.Path(self.temporary.name)
        self.old = time.time() - 8 * 3600

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def candidate(self, name: str, *, marked: bool = True) -> pathlib.Path:
        root = self.root / name
        root.mkdir()
        if marked:
            (root / janitor.REPRODUCER_MARKER).write_text("lvu test fixture\n")
        (root / "payload").write_bytes(b"payload")
        self.make_old(root)
        return root

    def make_old(self, root: pathlib.Path) -> None:
        for path in root.rglob("*"):
            os.utime(path, (self.old, self.old), follow_symlinks=False)
        os.utime(root, (self.old, self.old), follow_symlinks=False)

    def sweep(self, *, dry: bool = False, **kwargs: object) -> tuple[int, int]:
        return janitor.sweep_reproducers(
            3,
            dry,
            self.root,
            cwds=kwargs.pop("cwds", []),
            commands=kwargs.pop("commands", []),
            **kwargs,
        )

    def test_only_marked_owned_old_candidates_are_removed(self) -> None:
        removable = self.candidate("lvu-marked-old")
        unmarked = self.candidate("lvu-unmarked-user-data", marked=False)
        wrong_name = self.candidate("ordinary-test-tree")
        wrong_owner = self.candidate("w19-wrong-owner")

        freed, count = self.sweep(owner_uid=os.getuid() + 1)
        self.assertEqual((freed, count), (0, 0))
        self.assertTrue(wrong_owner.exists())

        wrong_owner.rename(self.root / "ordinary-owner-check")
        freed, count = self.sweep()
        self.assertGreater(freed, 0)
        self.assertEqual(count, 1)
        self.assertFalse(removable.exists())
        self.assertTrue(unmarked.exists())
        self.assertTrue(wrong_name.exists())

    def test_young_root_or_fresh_descendant_is_preserved(self) -> None:
        young = self.candidate("lvu-young")
        os.utime(young, None)
        fresh = self.candidate("w13-fresh-child")
        os.utime(fresh / "payload", None)

        self.assertEqual(self.sweep(), (0, 0))
        self.assertTrue(young.exists())
        self.assertTrue(fresh.exists())

    def test_cwd_and_command_line_each_preserve_a_candidate(self) -> None:
        cwd_used = self.candidate("lvu-cwd-used")
        command_used = self.candidate("w13-command-used")

        self.assertEqual(
            self.sweep(cwds=[str(cwd_used / "nested")], commands=[str(command_used)]),
            (0, 0),
        )
        self.assertTrue(cwd_used.exists())
        self.assertTrue(command_used.exists())

    def test_symlink_and_protected_root_or_nested_material_are_preserved(self) -> None:
        target = self.candidate("ordinary-target")
        symlink = self.root / "lvu-linked"
        symlink.symlink_to(target, target_is_directory=True)
        protected_root = self.candidate("lvu-proof-archive")
        protected_preview = self.candidate("w13-preview-archive")
        protected_nested = self.candidate("w13-nested-protected")
        (protected_nested / "captures").mkdir()
        self.make_old(protected_nested)

        self.assertEqual(self.sweep(), (0, 0))
        self.assertTrue(symlink.is_symlink())
        self.assertTrue(protected_root.exists())
        self.assertTrue(protected_preview.exists())
        self.assertTrue(protected_nested.exists())

    def test_dry_run_reports_but_deletes_nothing(self) -> None:
        candidate = self.candidate("lvu-dry-run")

        freed, count = self.sweep(dry=True)
        self.assertGreater(freed, 0)
        self.assertEqual(count, 1)
        self.assertTrue(candidate.exists())

    def test_failed_or_disappearing_removal_claims_no_bytes(self) -> None:
        candidate = self.candidate("lvu-remove-fails")
        with mock.patch.object(janitor.shutil, "rmtree", side_effect=OSError("busy")):
            self.assertEqual(self.sweep(), (0, 0))
        self.assertTrue(candidate.exists())

        disappearing = self.candidate("w13-disappearing")
        real_rmtree = janitor.shutil.rmtree

        def disappear_then_fail(path: pathlib.Path) -> None:
            real_rmtree(path)
            raise FileNotFoundError(path)

        with mock.patch.object(janitor.shutil, "rmtree", side_effect=disappear_then_fail):
            self.assertEqual(self.sweep(), (0, 0))
        self.assertFalse(disappearing.exists())


if __name__ == "__main__":
    unittest.main()
