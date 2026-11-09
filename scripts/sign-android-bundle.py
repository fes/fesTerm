#!/usr/bin/env python3
"""Sign an unsigned fesTerm Play AAB with the Play upload key.

Runs only the JDK's jarsigner, after checking the bundle against the SHA-256
recorded by the unprivileged build job, so dependency build scripts never run
while the upload key is present (ADR 0083).
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[1]
UPLOAD_KEYSTORE = "ANDROID_UPLOAD_KEYSTORE"
UPLOAD_PASSWORD = "ANDROID_UPLOAD_KEYSTORE_PASSWORD"
UPLOAD_ALIAS = "ANDROID_UPLOAD_KEY_ALIAS"
FINGERPRINT = re.compile(r"(?m)^\s*SHA256:\s*((?:[0-9A-Fa-f]{2}:){31}[0-9A-Fa-f]{2})\s*$")


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def upload_signing():
    """Resolves the Play upload key from the environment, never the development key."""
    missing = [name for name in (UPLOAD_KEYSTORE, UPLOAD_PASSWORD, UPLOAD_ALIAS) if not os.environ.get(name)]
    if missing:
        raise ValueError("Play bundles require " + ", ".join(missing))
    keystore = Path(os.environ[UPLOAD_KEYSTORE]).expanduser()
    if not keystore.is_file() or keystore.is_symlink():
        raise ValueError(f"Play upload keystore must be a regular file: {keystore}")
    keystore = keystore.resolve()
    if keystore == (ROOT / "target" / "android" / "development.keystore").resolve():
        raise ValueError("The development keystore must never sign a Play bundle")
    alias = os.environ[UPLOAD_ALIAS]
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,128}", alias) or alias.lower() == "androiddebugkey":
        raise ValueError("A non-development Android upload key alias is required")
    return keystore, alias


def run(*args, env):
    return subprocess.run(
        [str(arg) for arg in args], cwd=ROOT, env=env, check=True,
        text=True, capture_output=True, timeout=600,
    ).stdout


def fingerprint(output):
    found = FINGERPRINT.findall(output)
    if len(found) != 1:
        raise ValueError("Expected exactly one SHA-256 signing certificate fingerprint")
    return found[0].replace(":", "").upper()


def upload_identity(keystore, alias, env):
    details = run("keytool", "-list", "-v", "-keystore", keystore, "-alias", alias,
                  "-storepass:env", UPLOAD_PASSWORD, env=env)
    key = re.search(r"(?m)^Subject Public Key Algorithm:\s*([0-9,]+)-bit RSA key\s*$", details)
    if "Entry type: PrivateKeyEntry" not in details or not key or int(key.group(1).replace(",", "")) < 2048:
        raise ValueError("Play upload key must be a private RSA key of at least 2048 bits")
    return fingerprint(details)


def verify_signed_bundle(bundle, expected_fingerprint, env):
    result = subprocess.run(
        ["jarsigner", "-verify", "-strict", str(bundle)], cwd=ROOT, env=env,
        text=True, capture_output=True, timeout=600,
    )
    output = result.stdout + result.stderr
    if result.returncode == 4:
        error_block = output.partition("Error:")[2].split("\n\n", 1)[0].strip().splitlines()
        allowed = (
            "This jar contains entries whose certificate chain is invalid.",
            "This jar contains entries whose signer certificate is self-signed.",
        )
        if len(error_block) != 2 or not all(
            any(line.startswith(prefix) for prefix in allowed) for line in error_block
        ) or not any("self-signed" in line for line in error_block):
            raise ValueError("Android bundle signature has unexpected certificate errors")
    elif result.returncode != 0:
        raise ValueError(f"Android bundle signature verification failed (exit {result.returncode})")
    cert = run("keytool", "-printcert", "-jarfile", bundle, env=env)
    if fingerprint(cert) != expected_fingerprint:
        raise ValueError("Android bundle signer does not match the selected upload key")


def sign(unsigned, signed, expected_sha256, runner=subprocess.run):
    if not re.fullmatch(r"[0-9A-Fa-f]{64}", expected_sha256) or sha256(unsigned) != expected_sha256.lower():
        raise ValueError("Unsigned bundle does not match the build job's SHA-256")
    if signed.exists() or signed.is_symlink():
        raise ValueError("Refusing to overwrite an existing signed Android bundle")
    keystore, alias = upload_signing()
    env = dict(os.environ, LC_ALL="C")
    signer = upload_identity(keystore, alias, env)
    try:
        runner(["jarsigner", "-keystore", str(keystore),
                "-storepass:env", UPLOAD_PASSWORD, "-keypass:env", UPLOAD_PASSWORD,
                "-sigalg", "SHA256withRSA", "-digestalg", "SHA-256",
                "-signedjar", str(signed), str(unsigned), alias], check=True, env=env,
               text=True, capture_output=True, timeout=600)
        verify_signed_bundle(signed, signer, env)
    except (OSError, ValueError, subprocess.SubprocessError):
        signed.unlink(missing_ok=True)
        raise
    return {"unsigned_sha256": expected_sha256.lower(), "sha256": sha256(signed),
            "signer_sha256": signer}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("unsigned", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sha256", required=True, help="SHA-256 recorded by the build job")
    args = parser.parse_args()
    try:
        print(json.dumps(sign(args.unsigned, args.output, args.sha256), indent=2))
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Android bundle signing failed: {error}\n")


if __name__ == "__main__":
    main()
