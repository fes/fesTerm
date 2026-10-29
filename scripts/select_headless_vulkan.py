#!/usr/bin/env python3
"""Select the packaged software Vulkan driver for Linux headless tests."""

import argparse
import sys
from pathlib import Path


def configure(driver_directory: Path, github_env: Path) -> Path:
    drivers = sorted(
        path for path in driver_directory.glob("lvp_icd*.json") if path.is_file()
    )
    if len(drivers) != 1:
        raise ValueError(
            f"expected exactly one Lavapipe Vulkan ICD in {driver_directory}; "
            f"found {len(drivers)}"
        )
    driver = drivers[0].resolve(strict=True)
    with github_env.open("a", encoding="utf-8", newline="\n") as stream:
        stream.write(f"VK_DRIVER_FILES={driver}\n")
    return driver


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--github-env", type=Path, required=True)
    args = parser.parse_args()
    try:
        driver = configure(Path("/usr/share/vulkan/icd.d"), args.github_env)
    except (OSError, ValueError) as error:
        print(f"headless Vulkan: FAIL\n{error}", file=sys.stderr)
        return 1
    print(f"headless Vulkan: PASS ({driver})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
