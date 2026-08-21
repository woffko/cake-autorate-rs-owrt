#!/usr/bin/env python3
"""Run the final Full/Lite migration source gate serially.

The gate intentionally discovers the currently retained test files instead of
carrying the retired shell-suite names forward.  Every command runs to
completion before the next one starts because several lifecycle tests share
process and lock fixtures.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent
CARGO_MANIFEST = ROOT / "package/cake-autorate-rs/src/Cargo.toml"
FULL_LUCI = ROOT / "package/luci-app-cake-autorate-rs"
LITE_LUCI = ROOT / "package/luci-app-cake-autorate-rs-lite"


def run(label: str, argv: list[str]) -> None:
    started = time.monotonic()
    print(f"GATE_START {label}", flush=True)
    completed = subprocess.run(argv, cwd=ROOT, check=False)
    elapsed = time.monotonic() - started
    if completed.returncode != 0:
        print(
            f"GATE_FAIL {label} exit={completed.returncode} elapsed={elapsed:.1f}s",
            flush=True,
        )
        raise SystemExit(completed.returncode)
    print(f"GATE_PASS {label} elapsed={elapsed:.1f}s", flush=True)


def shell_sources() -> list[Path]:
    result: list[Path] = []
    for base in (ROOT / "package", ROOT / "scripts"):
        for path in base.rglob("*"):
            if not path.is_file():
                continue
            try:
                first = path.open("rb").readline(256)
            except OSError:
                continue
            if first.startswith(b"#!") and b"sh" in first:
                result.append(path)
    return sorted(result)


def main() -> int:
    def cargo(command: str, *arguments: str) -> list[str]:
        return [
            "cargo",
            command,
            "--manifest-path",
            str(CARGO_MANIFEST),
            *arguments,
        ]

    run("rust-fmt", cargo("fmt", "--all", "--", "--check"))
    run(
        "rust-full-tests",
        cargo("test", "--locked", "--", "--test-threads=1"),
    )
    run(
        "rust-lite-tests",
        cargo(
            "test",
            "--locked",
            "--no-default-features",
            "--",
            "--test-threads=1",
        ),
    )
    run("rust-full-check", cargo("check", "--locked"))
    run(
        "rust-lite-check",
        cargo("check", "--locked", "--no-default-features"),
    )

    for package in (
        ROOT / "package/cake-autorate-rs/tests",
        FULL_LUCI / "tests",
    ):
        for test in sorted(package.glob("*.test.sh")):
            run(f"shell-test:{test.name}", ["sh", str(test)])

    for package in (FULL_LUCI / "tests", LITE_LUCI / "tests"):
        for test in sorted(package.glob("*.test.js")):
            run(f"javascript-test:{test.name}", ["node", str(test)])

    run("typescript-full", ["npm", "--prefix", str(FULL_LUCI), "run", "check:types"])
    run(
        "typescript-lite",
        [
            str(FULL_LUCI / "node_modules/.bin/tsc"),
            "-p",
            str(LITE_LUCI / "tsconfig.json"),
        ],
    )

    for source in shell_sources():
        run(f"shell-syntax:{source.relative_to(ROOT)}", ["sh", "-n", str(source)])

    run("git-diff-check", ["git", "diff", "--check"])
    print("SOURCE_GATE_ACCEPTED FULL_LITE", flush=True)
    return 0


if __name__ == "__main__":
    os.chdir(ROOT)
    sys.exit(main())
