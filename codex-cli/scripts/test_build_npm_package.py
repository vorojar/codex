#!/usr/bin/env python3
"""Focused tests for Codex CLI npm package staging."""

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
BUILD_SCRIPT = SCRIPT_DIR / "build_npm_package.py"
APPLE_SILICON_TARGET = "aarch64-apple-darwin"


class BuildNpmPackageTests(unittest.TestCase):
    def test_darwin_arm64_package_includes_devicecheck_probe(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-npm-test-") as tmp_dir_str:
            tmp_dir = Path(tmp_dir_str)
            vendor_src = tmp_dir / "vendor"
            target_dir = vendor_src / APPLE_SILICON_TARGET

            (target_dir / "codex").mkdir(parents=True)
            (target_dir / "codex" / "codex").touch()
            (target_dir / "devicecheck-probe" / "DeviceCheckProbe.app").mkdir(parents=True)
            (target_dir / "path").mkdir(parents=True)
            (target_dir / "path" / "rg").touch()

            staging_dir = tmp_dir / "stage"
            subprocess.run(
                [
                    sys.executable,
                    str(BUILD_SCRIPT),
                    "--package",
                    "codex-darwin-arm64",
                    "--version",
                    "0.0.0-test",
                    "--staging-dir",
                    str(staging_dir),
                    "--vendor-src",
                    str(vendor_src),
                ],
                check=True,
            )

            staged_target_dir = staging_dir / "vendor" / APPLE_SILICON_TARGET
            self.assertTrue((staged_target_dir / "codex" / "codex").exists())
            self.assertTrue(
                (
                    staged_target_dir
                    / "devicecheck-probe"
                    / "DeviceCheckProbe.app"
                ).is_dir()
            )
            self.assertTrue((staged_target_dir / "path" / "rg").exists())


if __name__ == "__main__":
    unittest.main()
