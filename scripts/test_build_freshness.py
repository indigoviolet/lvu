#!/usr/bin/env python3
import contextlib
import importlib.util
import io
import json
import os
import pathlib
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("build-freshness.py")
spec = importlib.util.spec_from_file_location("freshness", SCRIPT)
freshness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(freshness)


class Tests(unittest.TestCase):
    def fixture(self, root):
        entries = []

        def add(name, deps=(), dev=None):
            directory = root / "crates" / name
            (directory / "src").mkdir(parents=True)
            (directory / "tests").mkdir()
            (directory / "benches").mkdir()
            manifest = directory / "Cargo.toml"
            source = directory / "src" / "lib.rs"
            manifest.write_text(f"[package]\nname='{name}'\nversion='0.0.0'\n")
            source.write_text("")
            (directory / "tests" / "integration.rs").write_text("")
            (directory / "benches" / "throughput.rs").write_text("")
            package = {
                "id": name,
                "manifest_path": str(manifest),
                "targets": [{"name": name, "kind": ["lib"], "src_path": str(source)}],
            }
            edges = [{"pkg": dep, "dep_kinds": [{"kind": None}]} for dep in deps]
            if dev:
                edges.append({"pkg": dev, "dep_kinds": [{"kind": "dev"}]})
            entry = {"package": package, "node": {"id": name, "deps": edges}}
            entries.append(entry)
            return entry

        linked_entry = add("linked")
        add("unlinked")
        app = add("lvu-app", ["linked"], "unlinked")
        demo = add("lvu")
        build_script = root / "crates" / "linked" / "build.rs"
        build_script.write_text("")
        linked_entry["package"]["targets"].append(
            {
                "name": "build-script-build",
                "kind": ["custom-build"],
                "src_path": str(build_script),
            }
        )
        for entry, name in ((app, "lvu-app"), (demo, "lvu")):
            path = root / "crates" / name / "src" / "main.rs"
            path.write_text("")
            entry["package"]["targets"].append(
                {"name": name, "kind": ["bin"], "src_path": str(path)}
            )
        (root / "Cargo.toml").write_text('[workspace]\nmembers=["crates/*"]\n')
        (root / "Cargo.lock").write_text("")
        return {
            "packages": [e["package"] for e in entries],
            "resolve": {"nodes": [e["node"] for e in entries]},
        }

    def test_linked_unlinked_and_test_sources(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            metadata = self.fixture(root)
            app = freshness.binary_inputs(metadata, root, "lvu-app")
            demo = freshness.binary_inputs(metadata, root, "lvu")
            linked = root / "crates/linked/src/lib.rs"
            unlinked = root / "crates/unlinked/src/lib.rs"
            test = root / "crates/linked/tests/integration.rs"
            bench = root / "crates/linked/benches/throughput.rs"
            self.assertIn(linked, app)
            self.assertNotIn(linked, demo)
            self.assertNotIn(unlinked, app)
            self.assertNotIn(test, app)
            self.assertNotIn(bench, app)
            self.assertIn(root / "crates/linked/build.rs", app)
            self.assertIn(root / "Cargo.toml", app)
            self.assertIn(root / "Cargo.lock", app)

    def test_cache_reuse_and_content_invalidation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            metadata = self.fixture(root)
            cache = root / "target/cache.json"
            calls = []
            loader = lambda _repo: calls.append(1) or metadata
            freshness.load_metadata(root, cache, loader)
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(1, len(calls))
            manifest = root / "crates/linked/Cargo.toml"
            manifest.write_text(manifest.read_text() + "# changed\n")
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(2, len(calls))

            added = root / "crates/added"
            added.mkdir()
            (added / "Cargo.toml").write_text(
                "[package]\nname='added'\nversion='0.0.0'\n"
            )
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(3, len(calls))

            unrelated = root / "previews" / "copied-source"
            unrelated.mkdir(parents=True)
            (unrelated / "Cargo.toml").write_text(
                "[package]\nname='unrelated'\nversion='0.0.0'\n"
            )
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(3, len(calls))

            path_dependency = root / "local-dependency"
            path_dependency.mkdir()
            dependency_manifest = path_dependency / "Cargo.toml"
            dependency_manifest.write_text(
                "[package]\nname='local-dependency'\nversion='0.0.0'\n"
            )
            manifest.write_text(
                manifest.read_text()
                + "[dependencies]\nlocal-dependency={path='../../local-dependency'}\n"
            )
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(4, len(calls))
            dependency_manifest.write_text(
                dependency_manifest.read_text() + "# changed\n"
            )
            freshness.load_metadata(root, cache, loader)
            self.assertEqual(5, len(calls))

    def test_preflight_observes_only_inputs_linked_to_each_binary(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            metadata = self.fixture(root)
            target = root / "target/debug"
            target.mkdir(parents=True)
            cache = target.parent / ".build-freshness-metadata.json"
            cache.write_text(
                json.dumps(
                    {
                        "version": freshness.CACHE_VERSION,
                        "key": freshness.metadata_key(root),
                        "metadata": metadata,
                    }
                )
            )
            binaries = [target / name for name in freshness.BINARIES]
            all_files = [path for path in root.rglob("*") if path.is_file()]

            def run_with_newer(path):
                for source in all_files:
                    os.utime(source, (100, 100))
                for binary in binaries:
                    binary.write_text("")
                    os.utime(binary, (200, 200))
                os.utime(path, (300, 300))
                old = sys.argv
                sys.argv = [str(SCRIPT), "--repo", str(root), "--target", str(target)]
                try:
                    with contextlib.redirect_stderr(io.StringIO()):
                        return freshness.main()
                finally:
                    sys.argv = old

            self.assertEqual(1, run_with_newer(root / "crates/linked/src/lib.rs"))
            self.assertEqual(1, run_with_newer(root / "crates/linked/build.rs"))
            self.assertEqual(0, run_with_newer(root / "crates/unlinked/src/lib.rs"))
            self.assertEqual(
                0, run_with_newer(root / "crates/linked/tests/integration.rs")
            )
            self.assertEqual(
                0, run_with_newer(root / "crates/linked/benches/throughput.rs")
            )
            self.assertEqual(1, run_with_newer(root / "Cargo.toml"))
            self.assertEqual(1, run_with_newer(root / "Cargo.lock"))


if __name__ == "__main__":
    unittest.main()
