#!/usr/bin/env python3
"""Keep every pdf-inspector package on one release version."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PRERELEASE_IDENTIFIER = (
    r"(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
)
SEMVER = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    rf"(?:-{PRERELEASE_IDENTIFIER}(?:\.{PRERELEASE_IDENTIFIER})*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)
VERSION_LINE = re.compile(r'^(\s*version\s*=\s*")[^"]+(".*)$')
SECTION_LINE = re.compile(r"^\s*\[([^]]+)]\s*$")
PLATFORM_PACKAGES = (
    "@firecrawl/pdf-inspector-linux-x64-gnu",
    "@firecrawl/pdf-inspector-linux-x64-musl",
    "@firecrawl/pdf-inspector-linux-arm64-gnu",
    "@firecrawl/pdf-inspector-linux-arm64-musl",
    "@firecrawl/pdf-inspector-darwin-arm64",
    "@firecrawl/pdf-inspector-win32-x64-msvc",
)
TOML_VERSIONS = (
    ("Rust crate", Path("Cargo.toml"), "package"),
    ("Python package", Path("pyproject.toml"), "project"),
    ("NAPI crate", Path("napi/Cargo.toml"), "package"),
    ("WASM package", Path("wasm/Cargo.toml"), "package"),
)
LOCK_VERSIONS = (
    ("NAPI lock: core", Path("napi/Cargo.lock"), "pdf-inspector"),
    ("NAPI lock: binding", Path("napi/Cargo.lock"), "pdf-inspector-napi"),
    ("WASM lock: core", Path("wasm/Cargo.lock"), "pdf-inspector"),
    ("WASM lock: binding", Path("wasm/Cargo.lock"), "pdf-inspector-wasm"),
)
SITE_WASM_VERSION = re.compile(
    r"(@firecrawl/pdf-inspector-wasm@)([^/\"]+)(/pdf_inspector_wasm\.js)"
)


def _read_section_version(path: Path, section: str) -> str:
    active = False
    for line in path.read_text(encoding="utf-8").splitlines():
        section_match = SECTION_LINE.match(line)
        if section_match:
            active = section_match.group(1) == section
        elif active:
            version_match = VERSION_LINE.match(line)
            if version_match:
                return line.split('"', 2)[1]
    raise ValueError(f"No version found in [{section}] of {path}")


def _write_section_version(path: Path, section: str, version: str) -> None:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    active = False
    for index, line in enumerate(lines):
        section_match = SECTION_LINE.match(line)
        if section_match:
            active = section_match.group(1) == section
        elif active:
            version_match = VERSION_LINE.match(line)
            if version_match:
                newline = "\n" if line.endswith("\n") else ""
                replacement = (
                    f"{version_match.group(1)}{version}"
                    f"{version_match.group(2).rstrip()}"
                )
                lines[index] = (
                    f"{replacement}{newline}"
                )
                path.write_text("".join(lines), encoding="utf-8")
                return
    raise ValueError(f"No version found in [{section}] of {path}")


def _package_block(lines: list[str], package: str) -> tuple[int, int]:
    for start, line in enumerate(lines):
        if line.strip() != "[[package]]":
            continue
        end = next(
            (
                index
                for index in range(start + 1, len(lines))
                if lines[index].strip() == "[[package]]"
            ),
            len(lines),
        )
        if any(line.strip() == f'name = "{package}"' for line in lines[start:end]):
            return start, end
    raise ValueError(f"No lockfile entry found for {package}")


def _read_lock_version(path: Path, package: str) -> str:
    lines = path.read_text(encoding="utf-8").splitlines()
    start, end = _package_block(lines, package)
    for line in lines[start:end]:
        version_match = VERSION_LINE.match(line)
        if version_match:
            return line.split('"', 2)[1]
    raise ValueError(f"No version found for {package} in {path}")


def _write_lock_version(path: Path, package: str, version: str) -> None:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    start, end = _package_block(lines, package)
    for index in range(start, end):
        version_match = VERSION_LINE.match(lines[index])
        if version_match:
            newline = "\n" if lines[index].endswith("\n") else ""
            lines[index] = (
                f'{version_match.group(1)}{version}{version_match.group(2).rstrip()}'
                f"{newline}"
            )
            path.write_text("".join(lines), encoding="utf-8")
            return
    raise ValueError(f"No version found for {package} in {path}")


def _node_versions(root: Path) -> dict[str, str]:
    package = json.loads((root / "napi/package.json").read_text(encoding="utf-8"))
    versions = {"Node package": package["version"]}
    optional = package.get("optionalDependencies", {})
    for dependency in PLATFORM_PACKAGES:
        if dependency not in optional:
            raise ValueError(f"Missing Node optional dependency: {dependency}")
        versions[f"Node optional dependency: {dependency}"] = optional[dependency]
    return versions


def _bun_versions(root: Path) -> dict[str, str]:
    text = (root / "napi/bun.lock").read_text(encoding="utf-8")
    versions = {}
    for dependency in PLATFORM_PACKAGES:
        match = re.search(
            rf'"{re.escape(dependency)}": "([^"]+)"[,]', text
        )
        if not match:
            raise ValueError(f"Missing Bun lock dependency: {dependency}")
        versions[f"Bun lock: {dependency}"] = match.group(1)
    return versions


def _site_wasm_version(root: Path) -> str:
    text = (root / "site/index.html").read_text(encoding="utf-8")
    match = SITE_WASM_VERSION.search(text)
    if not match:
        raise ValueError("Missing pinned WASM package URL in site/index.html")
    return match.group(2)


def package_versions(root: Path = ROOT) -> dict[str, str]:
    versions = {
        label: _read_section_version(root / relative, section)
        for label, relative, section in TOML_VERSIONS
    }
    versions.update(_node_versions(root))
    versions.update(_bun_versions(root))
    versions["Website WASM module"] = _site_wasm_version(root)
    versions.update(
        {
            label: _read_lock_version(root / relative, package)
            for label, relative, package in LOCK_VERSIONS
        }
    )
    return versions


def check_versions(root: Path = ROOT) -> str:
    versions = package_versions(root)
    expected = versions["Rust crate"]
    if not SEMVER.fullmatch(expected):
        raise ValueError(f"Rust crate has an invalid semantic version: {expected}")
    mismatches = {
        label: version for label, version in versions.items() if version != expected
    }
    if mismatches:
        details = "\n".join(
            f"  - {label}: {version}" for label, version in mismatches.items()
        )
        raise ValueError(f"Expected every package to use {expected}:\n{details}")
    return expected


def set_versions(version: str, root: Path = ROOT) -> None:
    if not SEMVER.fullmatch(version):
        raise ValueError(f"Invalid semantic version: {version}")

    # Validate every expected location before writing the first file. This
    # prevents a stale manifest or generated file from leaving a partial bump.
    package_versions(root)

    for _, relative, section in TOML_VERSIONS:
        _write_section_version(root / relative, section, version)

    package_path = root / "napi/package.json"
    package = json.loads(package_path.read_text(encoding="utf-8"))
    package["version"] = version
    optional = package.get("optionalDependencies", {})
    for dependency in PLATFORM_PACKAGES:
        if dependency not in optional:
            raise ValueError(f"Missing Node optional dependency: {dependency}")
        optional[dependency] = version
    package_path.write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")

    bun_path = root / "napi/bun.lock"
    bun_text = bun_path.read_text(encoding="utf-8")
    for dependency in PLATFORM_PACKAGES:
        pattern = rf'("{re.escape(dependency)}": ")[^"]+("[,])'
        bun_text, count = re.subn(
            pattern, rf"\g<1>{version}\g<2>", bun_text, count=1
        )
        if count != 1:
            raise ValueError(f"Missing Bun lock dependency: {dependency}")
    bun_path.write_text(bun_text, encoding="utf-8")

    site_path = root / "site/index.html"
    site_text = site_path.read_text(encoding="utf-8")
    site_text, count = SITE_WASM_VERSION.subn(
        rf"\g<1>{version}\g<3>", site_text, count=1
    )
    if count != 1:
        raise ValueError("Missing pinned WASM package URL in site/index.html")
    site_path.write_text(site_text, encoding="utf-8")

    for _, relative, package_name in LOCK_VERSIONS:
        _write_lock_version(root / relative, package_name, version)

    check_versions(root)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?", help="new shared semantic version")
    parser.add_argument(
        "--check", action="store_true", help="fail if package versions have diverged"
    )
    arguments = parser.parse_args()
    if arguments.check == bool(arguments.version):
        parser.error("provide either a version or --check")

    try:
        if arguments.check:
            version = check_versions()
            print(f"All packages use {version}")
        else:
            set_versions(arguments.version)
            print(f"Updated all packages to {arguments.version}")
    except ValueError as error:
        parser.exit(1, f"{error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
