#!/usr/bin/env python3
"""Sign an unsigned fesTerm Play AAB with the Play upload key.

Runs only the JDK's jarsigner, after checking the bundle against the SHA-256
recorded by the unprivileged build job, so dependency build scripts never run
while the upload key is present (ADR 0083).
"""

import argparse
import hashlib
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
UPLOAD_KEYSTORE = "ANDROID_UPLOAD_KEYSTORE"
UPLOAD_PASSWORD = "ANDROID_UPLOAD_KEYSTORE_PASSWORD"
UPLOAD_ALIAS = "ANDROID_UPLOAD_KEY_ALIAS"


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def upload_signing():
    """Resolves the Play upload key from the environment, never the development key."""
    missing = [name for name in (UPLOAD_KEYSTORE, UPLOAD_PASSWORD, UPLOAD_ALIAS) if not os.environ.get(name)]
    if missing:
        raise ValueError("Play bundles require " + ", ".join(missing))
    keystore = Path(os.environ[UPLOAD_KEYSTORE]).resolve()
    if not keystore.is_file():
        raise ValueError(f"Play upload keystore does not exist: {keystore}")
    if keystore == (ROOT / "target" / "android" / "development.keystore").resolve():
        raise ValueError("The development keystore must never sign a Play bundle")
    return keystore, os.environ[UPLOAD_ALIAS]


def sign(unsigned, signed, expected_sha256, runner=subprocess.run):
    if sha256(unsigned) != expected_sha256.lower():
        raise ValueError("Unsigned bundle does not match the build job's SHA-256")
    keystore, alias = upload_signing()
    signed.unlink(missing_ok=True)
    runner(["jarsigner", "-keystore", str(keystore), "-storetype", "PKCS12",
            "-storepass:env", UPLOAD_PASSWORD, "-keypass:env", UPLOAD_PASSWORD,
            "-sigalg", "SHA256withRSA", "-digestalg", "SHA-256",
            "-signedjar", str(signed), str(unsigned), alias], check=True)
    verification = runner(["jarsigner", "-verify", str(signed)], check=True,
                          text=True, stdout=subprocess.PIPE).stdout
    if "jar verified." not in verification:
        signed.unlink(missing_ok=True)
        raise ValueError("Play bundle signature did not verify")
    return sha256(signed)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("unsigned", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sha256", required=True, help="SHA-256 recorded by the build job")
    args = parser.parse_args()
    try:
        print(sign(args.unsigned, args.output, args.sha256))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Android bundle signing failed: {error}\n")


if __name__ == "__main__":
    main()
