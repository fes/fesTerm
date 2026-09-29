#!/usr/bin/env python3
"""Add release-only signing metadata to the checked-in macOS package config."""

from __future__ import annotations

import argparse
import json
import tomllib
from pathlib import Path


class ConfigError(ValueError):
    pass


def render_release_config(source: str, signing_identity: str) -> str:
    if not signing_identity:
        raise ConfigError("the signing identity must not be empty")

    config = tomllib.loads(source)
    macos = config.get("macos")
    if not isinstance(macos, dict):
        raise ConfigError("the package config must contain one [macos] table")
    if "signing-identity" in macos:
        raise ConfigError("the checked-in config must not contain a signing identity")

    header = "[macos]"
    matches = [
        index for index, line in enumerate(source.splitlines()) if line.strip() == header
    ]
    if len(matches) != 1:
        raise ConfigError("the package config must contain exactly one [macos] table")

    lines = source.splitlines(keepends=True)
    header_index = next(
        index for index, line in enumerate(lines) if line.strip() == header
    )
    assignment = f"signing-identity = {json.dumps(signing_identity)}\n"
    lines.insert(header_index + 1, assignment)
    rendered = "".join(lines)

    rendered_config = tomllib.loads(rendered)
    if rendered_config["macos"]["signing-identity"] != signing_identity:
        raise ConfigError("the rendered signing identity did not round-trip")
    return rendered


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--signing-identity", required=True)
    args = parser.parse_args()

    source = args.input.read_text(encoding="utf-8")
    rendered = render_release_config(source, args.signing_identity)
    args.output.write_text(rendered, encoding="utf-8")


if __name__ == "__main__":
    main()
