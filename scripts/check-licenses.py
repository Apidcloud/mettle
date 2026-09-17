#!/usr/bin/env python3
"""Verify Cargo.lock licence metadata and maintain the checked-in report."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path


ALLOWED_LICENSES = {
    "(MIT OR Apache-2.0) AND Unicode-3.0",
    "Apache-2.0 AND ISC",
    "Apache-2.0 OR ISC OR MIT",
    "Apache-2.0 OR MIT",
    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
    "BSD-3-Clause",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT OR Apache-2.0",
    "Unlicense OR MIT",
}


def packages(repository: Path) -> list[dict[str, object]]:
    command = ["cargo", "metadata", "--format-version", "1", "--locked"]
    metadata = json.loads(
        subprocess.check_output(command, cwd=repository, text=True)
    )
    return sorted(
        (package for package in metadata["packages"] if package["source"] is not None),
        key=lambda package: (package["name"], package["version"]),
    )


def render(packages: list[dict[str, object]]) -> str:
    lines = [
        "# Locked third-party Rust packages",
        "",
        "Generated from `Cargo.lock` by `scripts/check-licenses.py`. Do not edit manually.",
        "",
        "| Package | Version | Declared licence |",
        "| --- | --- | --- |",
    ]
    for package in packages:
        lines.append(
            f"| `{package['name']}` | `{package['version']}` | "
            f"`{package.get('license') or 'UNKNOWN'}` |"
        )
    lines.extend(
        [
            "",
            "All declared licences in this locked graph are on the project's permissive licence allowlist. The release-readiness milestone will additionally produce distributable notices and an SBOM.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="update the report")
    arguments = parser.parse_args()

    repository = Path(__file__).resolve().parent.parent
    locked_packages = packages(repository)
    rejected = [
        package
        for package in locked_packages
        if package.get("license") not in ALLOWED_LICENSES
    ]
    if rejected:
        details = ", ".join(
            f"{package['name']} {package['version']} ({package.get('license') or 'UNKNOWN'})"
            for package in rejected
        )
        raise SystemExit(f"unapproved dependency licences: {details}")

    report = render(locked_packages)
    report_path = repository / "docs" / "third-party-licenses.md"
    if arguments.write:
        report_path.write_text(report, encoding="utf-8")
    elif not report_path.exists() or report_path.read_text(encoding="utf-8") != report:
        raise SystemExit(
            "dependency licence report is stale; run scripts/check-licenses.py --write"
        )

    print(f"Approved {len(locked_packages)} locked third-party packages.")


if __name__ == "__main__":
    main()
