import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from version import PLATFORM_PACKAGES, check_versions, set_versions


class VersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "napi").mkdir()
        (self.root / "site").mkdir()
        (self.root / "wasm").mkdir()

        self._write_manifest("Cargo.toml", "package", "0.1.0")
        self._write_manifest("pyproject.toml", "project", "0.1.0")
        self._write_manifest("napi/Cargo.toml", "package", "0.1.0")
        self._write_manifest("wasm/Cargo.toml", "package", "0.1.0")

        package = {
            "name": "@firecrawl/pdf-inspector",
            "version": "0.1.0",
            "optionalDependencies": {
                dependency: "0.1.0" for dependency in PLATFORM_PACKAGES
            },
        }
        (self.root / "napi/package.json").write_text(
            json.dumps(package), encoding="utf-8"
        )
        (self.root / "napi/bun.lock").write_text(
            "\n".join(
                f'        "{dependency}": "0.1.0",'
                for dependency in PLATFORM_PACKAGES
            )
            + "\n",
            encoding="utf-8",
        )
        (self.root / "site/index.html").write_text(
            'https://cdn.jsdelivr.net/npm/@firecrawl/pdf-inspector-wasm@0.1.0/'
            'pdf_inspector_wasm.js\n',
            encoding="utf-8",
        )
        self._write_lock(
            "napi/Cargo.lock", ("pdf-inspector", "pdf-inspector-napi")
        )
        self._write_lock(
            "wasm/Cargo.lock", ("pdf-inspector", "pdf-inspector-wasm")
        )

    def tearDown(self):
        self.temporary.cleanup()

    def _write_manifest(self, relative, section, version):
        (self.root / relative).write_text(
            f'[{section}]\nname = "fixture"\nversion = "{version}"\n',
            encoding="utf-8",
        )

    def _write_lock(self, relative, packages):
        content = "\n".join(
            f'[[package]]\nname = "{package}"\nversion = "0.1.0"\n'
            for package in packages
        )
        (self.root / relative).write_text(content, encoding="utf-8")

    def test_updates_every_version_location(self):
        set_versions("1.14.0", self.root)

        self.assertEqual(check_versions(self.root), "1.14.0")

    def test_reports_a_divergent_package(self):
        self._write_manifest("wasm/Cargo.toml", "package", "0.2.0")

        with self.assertRaisesRegex(ValueError, "WASM package: 0.2.0"):
            check_versions(self.root)

    def test_rejects_an_invalid_version(self):
        with self.assertRaisesRegex(ValueError, "Invalid semantic version"):
            set_versions("next", self.root)

    def test_rejects_numeric_prerelease_with_leading_zero(self):
        before = (self.root / "Cargo.toml").read_text(encoding="utf-8")

        with self.assertRaisesRegex(ValueError, "Invalid semantic version"):
            set_versions("1.2.3-01", self.root)

        self.assertEqual(
            (self.root / "Cargo.toml").read_text(encoding="utf-8"), before
        )

    def test_preflight_failure_does_not_partially_update(self):
        before = (self.root / "Cargo.toml").read_text(encoding="utf-8")
        (self.root / "site/index.html").write_text(
            "missing module URL\n", encoding="utf-8"
        )

        with self.assertRaisesRegex(ValueError, "Missing pinned WASM package URL"):
            set_versions("1.14.0", self.root)

        self.assertEqual(
            (self.root / "Cargo.toml").read_text(encoding="utf-8"), before
        )


if __name__ == "__main__":
    unittest.main()
