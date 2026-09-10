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


class AbandonedTargetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="janitor-targets-")
        self.root = pathlib.Path(self.temporary.name)
        self.targets = self.root / "build"
        self.worktrees = self.root / "worktrees"
        self.targets.mkdir()
        self.worktrees.mkdir()
        self.old = time.time() - 8 * 3600
        self.build_patch = mock.patch.object(janitor, "BUILD_VOLUME", self.targets)
        self.worktree_patch = mock.patch.object(janitor, "WORKTREES", self.worktrees)
        self.build_patch.start()
        self.worktree_patch.start()

    def tearDown(self) -> None:
        self.worktree_patch.stop()
        self.build_patch.stop()
        self.temporary.cleanup()

    def target(self, name: str) -> pathlib.Path:
        target = self.targets / f"{name}{janitor.TARGET_SUFFIX}"
        (target / "debug").mkdir(parents=True)
        return target

    def owner(self, name: str, *, present: bool = True) -> pathlib.Path:
        owner = self.worktrees / name
        if present:
            owner.mkdir()
        return owner

    def dep_info(
        self,
        target: pathlib.Path,
        *owners: pathlib.Path,
        text: str | None = None,
    ) -> None:
        path = target / "debug" / "lvu-app.d"
        if text is None:
            dependencies = " ".join(f"{owner}/crates/lvu/src/lib.rs" for owner in owners)
            text = f"{target}/debug/lvu-app: {dependencies}\n"
        path.write_text(text)
        os.utime(path, (self.old, self.old))
        os.utime(target / "debug", (self.old, self.old))
        os.utime(target, (self.old, self.old))

    def test_actual_custom_colour_target_uses_its_live_enrichment_owner(self) -> None:
        target = self.target("lvu-muse-colour")
        owner = self.owner("lvu-muse-enrichment")
        self.dep_info(target, owner)

        with mock.patch.object(janitor, "branch_is_merged", return_value=None) as merged:
            self.assertEqual(janitor.abandoned_targets(6), [])
        merged.assert_called_once_with(owner)

    def test_single_recorded_owner_that_is_gone_is_reclaimed(self) -> None:
        target = self.target("custom-name")
        owner = self.owner("finished-assignment", present=False)
        self.dep_info(target, owner)

        self.assertEqual(
            janitor.abandoned_targets(6),
            [(target, "recorded worktree finished-assignment is gone")],
        )

    def test_multiple_or_unknown_ownership_is_retained(self) -> None:
        first = self.owner("first")
        second = self.owner("second")
        ambiguous = self.target("ambiguous")
        self.dep_info(ambiguous, first, second)

        self.target("unknown")
        malformed = self.target("malformed")
        self.dep_info(malformed, text="truncated dep info")
        escaped = self.target("escaped")
        self.dep_info(
            escaped,
            text=f"{escaped}/debug/lvu-app: {first}/crates/lvu/src/escaped\\ path.rs\n",
        )

        self.assertEqual(janitor.abandoned_targets(6), [])

    def test_dep_info_read_count_and_bytes_are_bounded(self) -> None:
        owner = self.owner("bounded")
        too_many = self.target("too-many")
        too_large = self.target("too-large")
        with mock.patch.object(janitor, "TARGET_OWNER_DEP_INFO_FILES", 1):
            self.dep_info(too_many, owner)
            (too_many / "debug" / "second.d").write_text(
                f"out: {owner}/crates/lvu/src/lib.rs\n"
            )
            self.assertIsNone(janitor.target_worktree_owners(too_many))
        with mock.patch.object(janitor, "TARGET_OWNER_DEP_INFO_BYTES", 16):
            self.dep_info(too_large, owner)
            self.assertIsNone(janitor.target_worktree_owners(too_large))

    def test_non_descendant_and_lexically_escaping_paths_are_unknown(self) -> None:
        malformed_dependencies = {
            "owner-root": f"{self.worktrees}/gone",
            "current": f"{self.worktrees}/gone/./source.rs",
            "parent": f"{self.worktrees}/gone/../outside/source.rs",
            "double-parent": f"{self.worktrees}/gone/../../outside/source.rs",
            "double-slash": f"{self.worktrees}/gone//crates/lvu/src/lib.rs",
            "control": f"{self.worktrees}/gone/crates/\x00source.rs",
        }
        for name, dependency in malformed_dependencies.items():
            with self.subTest(name=name):
                target = self.target(name)
                self.dep_info(target, text=f"out: {dependency}\n")
                self.assertIsNone(janitor.target_worktree_owners(target))

        self.assertEqual(janitor.abandoned_targets(6), [])

    def test_all_examined_directory_entries_share_a_strict_bound(self) -> None:
        excess_top_level = self.target("excess-top-level")
        (excess_top_level / "ordinary-one").write_text("not a profile")
        (excess_top_level / "ordinary-two").write_text("not a profile")

        excess_profile = self.target("excess-profile")
        (excess_profile / "debug" / "ordinary-one").write_text("not dep-info")
        (excess_profile / "debug" / "ordinary-two").write_text("not dep-info")

        with mock.patch.object(janitor, "TARGET_OWNER_ENTRIES", 2):
            self.assertIsNone(janitor.target_worktree_owners(excess_top_level))
            self.assertIsNone(janitor.target_worktree_owners(excess_profile))
            self.assertEqual(janitor.abandoned_targets(6), [])

    def test_entry_scan_is_lazy_and_stops_at_the_aggregate_cap(self) -> None:
        class NonProfileEntry:
            def __init__(self, number: int) -> None:
                self.name = f"ordinary-{number}"
                self.path = f"/unused/{self.name}"

            def is_dir(self, *, follow_symlinks: bool) -> bool:
                if follow_symlinks:
                    raise AssertionError("ownership scan followed a symlink")
                return False

        class LazyScandir:
            def __init__(self, limit: int) -> None:
                self.limit = limit
                self.consumed = 0
                self.closed = False

            def __enter__(self) -> "LazyScandir":
                return self

            def __exit__(self, *args: object) -> None:
                self.closed = True

            def __iter__(self) -> "LazyScandir":
                return self

            def __next__(self) -> NonProfileEntry:
                if self.consumed >= self.limit:
                    raise AssertionError("ownership scan consumed beyond its cap")
                self.consumed += 1
                return NonProfileEntry(self.consumed)

        entries = LazyScandir(3)
        with (
            mock.patch.object(janitor, "TARGET_OWNER_ENTRIES", 3),
            mock.patch.object(janitor.os, "scandir", return_value=entries) as scandir,
        ):
            self.assertIsNone(janitor.target_worktree_owners(self.targets / "custom"))

        self.assertEqual(entries.consumed, 3)
        self.assertTrue(entries.closed)
        scandir.assert_called_once_with(self.targets / "custom")

    def test_existing_conventional_worktree_keeps_merged_idle_policy(self) -> None:
        target = self.target("conventional")
        owner = self.owner("conventional")
        self.dep_info(target, text=f"{target}/debug/output: relative/source.rs\n")

        with mock.patch.object(
            janitor, "branch_is_merged", return_value="merged-branch"
        ):
            found = janitor.abandoned_targets(6)
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0][0], target)
        self.assertIn("branch merged-branch is in main", found[0][1])


if __name__ == "__main__":
    unittest.main()
