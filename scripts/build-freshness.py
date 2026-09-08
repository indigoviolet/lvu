#!/usr/bin/env python3
"""Refuse a PTY matrix that would run binaries older than their own inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import tomllib
from typing import Any, Callable

REPO = pathlib.Path(__file__).resolve().parent.parent
BINARIES = ("lvu", "lvu-app")
MAX_NAMED = 5
CACHE_VERSION = 1


def metadata_inputs(repo: pathlib.Path) -> list[pathlib.Path]:
    """Manifests capable of changing this workspace's metadata graph."""
    root_manifest = repo / "Cargo.toml"
    pending = [root_manifest]
    found: set[pathlib.Path] = set()
    while pending:
        manifest = pending.pop()
        if manifest in found or not manifest.is_file():
            continue
        found.add(manifest)
        try:
            document = tomllib.loads(manifest.read_text())
        except (OSError, tomllib.TOMLDecodeError):
            continue

        workspace = document.get("workspace", {})
        if manifest == root_manifest:
            excluded = {
                path.resolve()
                for pattern in workspace.get("exclude", [])
                for path in repo.glob(pattern)
            }
            for pattern in workspace.get("members", []):
                for member in repo.glob(pattern):
                    candidate = (member / "Cargo.toml").resolve()
                    if member.resolve() not in excluded:
                        pending.append(candidate)

        def visit(value: Any) -> None:
            if isinstance(value, dict):
                dependency_path = value.get("path")
                if isinstance(dependency_path, str):
                    dependency = (manifest.parent / dependency_path).resolve()
                    pending.append(
                        dependency
                        if dependency.name == "Cargo.toml"
                        else dependency / "Cargo.toml"
                    )
                for child in value.values():
                    visit(child)
            elif isinstance(value, list):
                for child in value:
                    visit(child)

        visit(document)
    return sorted(found) + [repo / "Cargo.lock"]


def metadata_key(repo: pathlib.Path) -> str:
    digest = hashlib.sha256()
    for path in metadata_inputs(repo):
        try:
            label = path.relative_to(repo)
        except ValueError:
            label = path
        digest.update(str(label).encode())
        digest.update(b"\0")
        try:
            digest.update(path.read_bytes())
        except OSError:
            digest.update(b"<missing>")
        digest.update(b"\0")
    return digest.hexdigest()


def run_metadata(repo: pathlib.Path) -> dict[str, Any]:
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=repo,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(result.stdout)


def load_metadata(
    repo: pathlib.Path,
    cache: pathlib.Path,
    loader: Callable[[pathlib.Path], dict[str, Any]] = run_metadata,
) -> dict[str, Any]:
    key = metadata_key(repo)
    try:
        saved = json.loads(cache.read_text())
        if saved.get("version") == CACHE_VERSION and saved.get("key") == key:
            return saved["metadata"]
    except (OSError, ValueError, KeyError, TypeError):
        pass
    metadata = loader(repo)
    cache.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile("w", dir=cache.parent, delete=False) as output:
            json.dump(
                {"version": CACHE_VERSION, "key": key, "metadata": metadata}, output
            )
            temporary = pathlib.Path(output.name)
        temporary.replace(cache)
    except OSError:
        if temporary:
            try:
                temporary.unlink()
            except OSError:
                pass
    return metadata


def non_dev_dependencies(node: dict[str, Any]) -> set[str]:
    return {
        dep["pkg"]
        for dep in node.get("deps", [])
        if not dep.get("dep_kinds")
        or any(kind.get("kind") != "dev" for kind in dep["dep_kinds"])
    }


def dependency_closure(metadata: dict[str, Any], binary: str) -> set[str]:
    packages = {package["id"]: package for package in metadata["packages"]}
    pending = [
        package_id
        for package_id, package in packages.items()
        if any(
            binary == target["name"] and "bin" in target["kind"]
            for target in package["targets"]
        )
    ]
    if len(pending) != 1:
        raise RuntimeError(
            f"cargo metadata found {len(pending)} binary targets named {binary!r}"
        )
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    closure = set()
    while pending:
        package_id = pending.pop()
        if package_id in closure:
            continue
        closure.add(package_id)
        pending.extend(non_dev_dependencies(nodes[package_id]) - closure)
    return closure


def package_sources(package: dict[str, Any], binary: str | None) -> list[pathlib.Path]:
    manifest = pathlib.Path(package["manifest_path"])
    found = [manifest]
    roots = []
    for target in package["targets"]:
        kinds = set(target["kind"])
        if (
            "lib" in kinds
            or "custom-build" in kinds
            or (binary and "bin" in kinds and target["name"] == binary)
        ):
            source = pathlib.Path(target["src_path"])
            found.append(source)
            # A build script normally sits at the package root. Recursing from
            # there would pull in tests, benches and examples that are separate
            # Cargo targets, so only module-bearing library/bin roots recurse.
            if "custom-build" not in kinds:
                roots.append(source.parent if source.parent.name != "bin" else source)
    for root in roots:
        if root.is_dir():
            found.extend(root.rglob("*.rs"))
    return found


def binary_inputs(
    metadata: dict[str, Any], repo: pathlib.Path, binary: str
) -> list[pathlib.Path]:
    packages = {package["id"]: package for package in metadata["packages"]}
    closure = dependency_closure(metadata, binary)
    root_id = next(
        package_id
        for package_id in closure
        if any(
            binary == target["name"] and "bin" in target["kind"]
            for target in packages[package_id]["targets"]
        )
    )
    found = [repo / "Cargo.toml", repo / "Cargo.lock"]
    for package_id in closure:
        package = packages[package_id]
        manifest = pathlib.Path(package["manifest_path"])
        try:
            manifest.relative_to(repo)
        except ValueError:
            continue
        found.extend(
            package_sources(package, binary if package_id == root_id else None)
        )
    return sorted(set(path for path in found if path.is_file()))


def newest(paths: list[pathlib.Path]) -> float:
    return max((path.stat().st_mtime for path in paths), default=0.0)


def relative(path: pathlib.Path, repo: pathlib.Path) -> str:
    try:
        return str(path.relative_to(repo))
    except ValueError:
        return str(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target", default=os.environ.get("CARGO_TARGET_DIR", "target") + "/debug"
    )
    parser.add_argument(
        "--repo", type=pathlib.Path, default=REPO, help=argparse.SUPPRESS
    )
    args = parser.parse_args()
    repo = args.repo.resolve()
    target = pathlib.Path(args.target)
    try:
        metadata = load_metadata(repo, target.parent / ".build-freshness-metadata.json")
    except (
        OSError,
        subprocess.CalledProcessError,
        json.JSONDecodeError,
        RuntimeError,
    ) as error:
        print(
            f"Refusing the matrix: cannot determine binary inputs: {error}",
            file=sys.stderr,
        )
        return 1
    stale, missing, newer = [], [], {}
    for name in BINARIES:
        binary = target / name
        paths = binary_inputs(metadata, repo, name)
        if not binary.is_file():
            missing.append(name)
            continue
        stamp = binary.stat().st_mtime
        if stamp < newest(paths):
            stale.append((name, f"{newest(paths) - stamp:.0f}s older"))
            newer[name] = sorted(
                (p for p in paths if p.stat().st_mtime > stamp),
                key=lambda p: p.stat().st_mtime,
                reverse=True,
            )
    if not stale and not missing:
        return 0
    print(f"Refusing the matrix: {target} does not match the sources.", file=sys.stderr)
    for name in missing:
        print(f"  {name}: not built", file=sys.stderr)
    for name, age in stale:
        print(f"  {name}: {age} than its newest input", file=sys.stderr)
        for index, path in enumerate(newer[name][:MAX_NAMED]):
            print(
                f"    {'newest input' if index == 0 else '            '}: {relative(path, repo)}",
                file=sys.stderr,
            )
        if len(newer[name]) > MAX_NAMED:
            print(
                f"                  … and {len(newer[name]) - MAX_NAMED} more",
                file=sys.stderr,
            )
    print(
        "  Build into this target, or export the CARGO_TARGET_DIR the build used.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
