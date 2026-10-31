#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() { printf 'fesTerm tunnel setup: %s\n' "$*" >&2; exit 1; }
usage='Usage: bash install-dev-tunnel.sh [VERSION] [--install-dependencies] [--no-takeover]'
version=
install_dependencies=false
setup_extra=()
for arg in "$@"; do
    if [[ $arg == --install-dependencies ]]; then
        install_dependencies=true
    elif [[ $arg == --no-takeover ]]; then
        setup_extra=(--no-takeover)
    elif [[ $arg =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && -z $version ]]; then
        version=$arg
    else
        fail "$usage"
    fi
done
if [[ -z $version ]]; then
    # The newest published release; its redirect names the tag without the API.
    command -v curl >/dev/null || fail 'Missing curl; install the runtime prerequisites first.'
    latest=$(curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 20 --max-time 60 --output /dev/null --write-out '%{url_effective}' \
        https://github.com/fes/fesTerm/releases/latest) || fail 'Could not look up the latest release.'
    version=${latest##*/v}
    [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'No published fesTerm release was found; pass VERSION explicitly.'
fi
[[ $(id -u) != 0 ]] || fail 'Run as your normal user, not root.'
case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
        platform=linux; arch=x86_64; cli_kind=linux-x64
        cli_sha=723d6aebdc226064aef321258fb17c8a2770c9cc2bd05004c0fb83e72b302c3f ;;
    Linux:aarch64|Linux:arm64)
        platform=linux; arch=aarch64; cli_kind=linux-arm64
        cli_sha=b537f270a55d57f23b7621cec0b09806aa9ada119713b33e9255284179b76bde ;;
    Darwin:x86_64)
        platform=macos; arch=x86_64; cli_kind=osx-x64-zip
        cli_file_sha=39e5cbfbb5b21a94f14bfa1500d7e531dc90ae047bfe96146c4e05ab5ae4a18c
        cli_sha=e4a430bf2e8963fb6aec53be4a36a1dff40154369acb05906b822c47e439cae4 ;;
    Darwin:arm64)
        platform=macos; arch=aarch64; cli_kind=osx-arm64-zip
        cli_file_sha=7cac9a23cbb5e7c17b33d38b9921c8b0c108337533980bdb24d56c0c6e4b54fe
        cli_sha=f2fd04e1746345c7a009c17d6091b85b80b2633e0565623f851a8a298e8b56eb ;;
    *) fail 'Supported hosts: glibc Linux and macOS, x86_64 or ARM64.' ;;
esac
if [[ $platform == linux ]]; then cli_file_sha=$cli_sha; fi
if [[ $platform == linux ]]; then
    libc=$(getconf GNU_LIBC_VERSION) || fail 'glibc Linux is required; musl/Alpine is unsupported.'
    awk -v v="${libc#glibc }" 'BEGIN {split(v,a,"."); exit !(a[1]>2 || (a[1]==2 && a[2]>=39))}' ||
        fail 'The helper release requires glibc 2.39 or newer (Ubuntu 24.04+, Debian 13+).'
    if "$install_dependencies"; then
        printf 'Installing runtime prerequisites with sudo; no compiler or service is installed.\n'
        if command -v apt-get >/dev/null; then
            sudo apt-get update
            icu=$(apt-cache pkgnames | grep -E '^libicu[0-9]+$' | sort -V | tail -n 1)
            [[ -n $icu ]] || fail 'No supported ICU runtime package is available.'
            sudo apt-get install -y ca-certificates curl tar gzip libsecret-1-0 "$icu"
        elif command -v dnf >/dev/null; then
            sudo dnf install -y ca-certificates curl tar gzip libsecret libicu openssl-libs zlib libstdc++
        else
            fail 'Automatic runtime dependency installation supports apt-get or dnf only.'
        fi
    fi
fi
for tool in curl tar sort cmp; do
    command -v "$tool" >/dev/null || fail "Missing $tool; install the runtime prerequisites first."
done
if command -v sha256sum >/dev/null; then
    sha() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null; then
    sha() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    fail 'A SHA-256 tool (sha256sum or shasum) is required.'
fi
stage=$(mktemp -d "${TMPDIR:-/tmp}/festerm-tunnel.XXXXXXXX")
cleanup() {
    rm -f -- "$stage/bundle" "$stage/checksum" "$stage/cli" "$stage/expected" "$stage/actual"
    rmdir -- "$stage"
    if [[ -n ${candidate:-} && -d $candidate && ! -L $candidate ]]; then
        rm -f -- "$candidate/LICENSE" "$candidate/THIRD_PARTY_NOTICES.txt" "$candidate/VERSION" \
            "$candidate/festerm-sessiond" "$candidate/festerm-tunnel-bridge" "$candidate/festerm-tunnel-setup" "$candidate/devtunnel"
        rmdir -- "$candidate"
    fi
    if [[ -n ${cache_lock:-} ]]; then rmdir -- "$cache_lock"; fi
}
trap cleanup EXIT
fetch() {
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 20 --max-time 180 --max-filesize "$3" "$1" -o "$2"
}
asset="fesTerm-tunnel-${version}-${platform}-${arch}.tar.gz"
base="https://github.com/fes/fesTerm/releases/download/v${version}"
fetch "$base/$asset.sha256" "$stage/checksum" 1024 ||
    fail "Release v$version has no Dev Tunnel helper bundle for $platform-$arch. Pass a version that publishes one."
[[ $(wc -l < "$stage/checksum" | tr -d ' ') == 1 ]] || fail 'Malformed archive checksum.'
read -r digest filename < "$stage/checksum"
[[ $digest =~ ^[a-f0-9]{64}$ && $filename == "$asset" ]] || fail 'Checksum does not name this exact release.'
fetch "$base/$asset" "$stage/bundle" 536870912
[[ $(sha "$stage/bundle") == "$digest" ]] || fail 'Helper archive SHA-256 mismatch; nothing executed.'
printf '%s\n' LICENSE THIRD_PARTY_NOTICES.txt VERSION festerm-sessiond festerm-tunnel-bridge festerm-tunnel-setup |
    LC_ALL=C sort > "$stage/expected"
tar -tzf "$stage/bundle" | LC_ALL=C sort > "$stage/actual"
cmp -s "$stage/expected" "$stage/actual" || fail 'Helper archive has unexpected, duplicated or missing entries.'
if tar -tvzf "$stage/bundle" | grep -qv '^-'; then
    fail 'Helper archive contains a non-regular file.'
fi
root="$HOME/.local/share/festerm-tunnels"
[[ ! -L $root ]] || fail 'Install directory must not be a symbolic link.'
mkdir -p -- "$root"
chmod 700 "$root"
if [[ $platform == macos ]]; then
    chmod -N "$root"
fi
root=$(CDPATH='' cd -- "$root" && pwd -P)
mkdir -p -- "$root/releases"
chmod 700 "$root/releases"
requested_lock="$root/releases/$version-$platform-$arch.lock"
mkdir -- "$requested_lock" || fail 'Another installer (or an interrupted cache install) owns this release lock; no resources changed.'
cache_lock=$requested_lock
candidate=$(mktemp -d "$root/releases/.staging.XXXXXXXX")
install=$candidate
cached="$root/releases/$version-$platform-$arch"
tar -xzf "$stage/bundle" -C "$install"
printf '%s\n' "$version" | cmp -s - "$install/VERSION" || fail 'Bundle version mismatch.'
for binary in festerm-sessiond festerm-tunnel-bridge festerm-tunnel-setup; do
    chmod 700 "$install/$binary"
    if [[ $platform == macos ]]; then
        codesign --verify --strict "$install/$binary"
    fi
done
# Official Microsoft download bytes pinned on 2026-10-30. An upstream change
# refuses until reviewed pins are updated; do not silently accept new bytes.
if [[ -e $cached || -L $cached ]]; then
    [[ -d $cached && ! -L $cached && -f $cached/devtunnel && ! -L $cached/devtunnel ]] ||
        fail 'Cached official CLI is missing or substituted.'
    [[ $(sha "$cached/devtunnel") == "$cli_file_sha" ]] || fail 'Cached official CLI hash mismatch; refusing to overwrite live helpers.'
    cp -- "$cached/devtunnel" "$install/devtunnel"
else
    fetch "https://aka.ms/TunnelsCliDownload/$cli_kind" "$stage/cli" 134217728
    [[ $(sha "$stage/cli") == "$cli_sha" ]] || fail 'Official Dev Tunnels CLI hash changed; update reviewed pins before continuing.'
    if [[ $platform == macos ]]; then
        [[ $(unzip -Z1 "$stage/cli") == devtunnel ]] || fail 'Unexpected official CLI archive.'
        unzip -p "$stage/cli" devtunnel > "$install/devtunnel"
    else
        cp -- "$stage/cli" "$install/devtunnel"
    fi
fi
[[ $(sha "$install/devtunnel") == "$cli_file_sha" ]] || fail 'Official CLI executable hash mismatch.'
if [[ $platform == macos ]]; then codesign --verify --strict "$install/devtunnel"; fi
chmod 700 "$install/devtunnel"
"$install/devtunnel" --version
if [[ -e $cached || -L $cached ]]; then
    [[ -d $cached && ! -L $cached ]] || fail 'Cached release is not a regular directory.'
    for member in LICENSE THIRD_PARTY_NOTICES.txt VERSION festerm-sessiond festerm-tunnel-bridge festerm-tunnel-setup devtunnel; do
        [[ -f $cached/$member && ! -L $cached/$member ]] || fail 'Cached release has a missing or substituted file.'
        cmp -s "$install/$member" "$cached/$member" || fail 'Cached release differs from the verified release; refusing to overwrite live helpers.'
    done
    rm -f -- "$install/LICENSE" "$install/THIRD_PARTY_NOTICES.txt" "$install/VERSION" \
        "$install/festerm-sessiond" "$install/festerm-tunnel-bridge" "$install/festerm-tunnel-setup" "$install/devtunnel"
    rmdir -- "$install"
else
    mv -- "$install" "$cached"
fi
install=$cached
printf 'Installed private helpers: %s\n' "$install"
printf 'Setup reuses this user installation and its exact tunnel/session; a dedicated per-user service hosts it.\n'
cleanup
trap - EXIT
"$install/festerm-tunnel-setup" --managed --devtunnel "$install/devtunnel" --state-dir "$root/state" ${setup_extra[@]+"${setup_extra[@]}"}
profile="$root/state/festerm-profile.json"
[[ -f $profile && ! -L $profile ]] || fail 'Setup finished but did not write the connection JSON.'
copied=false
for clip in 'pbcopy' 'wl-copy' 'xclip -selection clipboard' 'xsel --clipboard --input'; do
    if command -v "${clip%% *}" >/dev/null && $clip < "$profile" 2>/dev/null; then copied=true; break; fi
done
printf '\n=== fesTerm connection JSON (public; no credentials) ===\n'
cat -- "$profile"
printf '\n=== Saved to %s ===\n' "$profile"
if "$copied"; then printf 'Copied to the clipboard. '; fi
printf 'In fesTerm: Dev Tunnel profile > Import setup JSON, paste, then sign in with the same GitHub account.\n'
