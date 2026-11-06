#!/usr/bin/env bash
set -euo pipefail
umask 077

fail() { printf 'fesTerm tunnel setup: %s\n' "$*" >&2; exit 1; }
usage='Usage: bash install-dev-tunnel.sh [VERSION] [--install-dependencies] [--no-takeover] [--upgrade] [--repair-session] [--friendly-name NAME] [--restart-unexpected | --no-restart-unexpected] [--file-root ABSOLUTE_DIRECTORY] [--allow-file-write] [--disable-files] [--observe-root ABSOLUTE_DIRECTORY] [--disable-observation]
       bash install-dev-tunnel.sh [VERSION] --manage {list|delete UUID --yes [--confirm-legacy-cloud-id QUALIFIED_ID]|delete-all --yes|archive UUID --yes} [--json] [--install-management-tools]'
version=
install_dependencies=false
no_takeover=false
upgrade=false
repair_session=false
friendly_name=
file_root=
file_write=false
disable_files=false
observe_roots=()
disable_observation=false
restart_unexpected=
manage=
installation_id=
yes=false
json=false
confirm_legacy_cloud_id=
install_management_tools=false
while (($#)); do
    if [[ $1 == --manage && $# -ge 2 && -z $manage ]]; then
        manage=$2
        shift 2
        if [[ ( $manage == delete || $manage == archive ) && $# -ge 1 && $1 != --* ]]; then
            installation_id=$1
            shift
        fi
    elif [[ $1 == --yes ]] && ! "$yes"; then
        yes=true
        shift
    elif [[ $1 == --json ]] && ! "$json"; then
        json=true
        shift
    elif [[ $1 == --confirm-legacy-cloud-id && $# -ge 2 && -z $confirm_legacy_cloud_id ]]; then
        confirm_legacy_cloud_id=$2
        shift 2
    elif [[ $1 == --install-management-tools ]] && ! "$install_management_tools"; then
        install_management_tools=true
        shift
    elif [[ $1 == --install-dependencies ]]; then
        install_dependencies=true
        shift
    elif [[ $1 == --no-takeover ]]; then
        no_takeover=true
        shift
    elif [[ $1 == --file-root && $# -ge 2 && -n $2 && -z $file_root ]]; then
        file_root=$2
        shift 2
    elif [[ $1 == --allow-file-write ]] && ! "$file_write"; then
        file_write=true
        shift
    elif [[ $1 == --disable-files ]] && ! "$disable_files"; then
        disable_files=true
        shift
    elif [[ $1 == --observe-root && $# -ge 2 && -n $2 && ${#observe_roots[@]} -lt 4 ]]; then
        observe_roots+=("$2")
        shift 2
    elif [[ $1 == --disable-observation ]] && ! "$disable_observation"; then
        disable_observation=true
        shift
    elif [[ $1 == --restart-unexpected || $1 == --no-restart-unexpected ]]; then
        [[ -z $restart_unexpected ]] || fail 'Choose only one unexpected-failure restart policy.'
        restart_unexpected=$1
        shift
    elif [[ $1 == --repair-session ]] && ! "$repair_session"; then
        repair_session=true
        shift
    elif [[ $1 == --upgrade ]]; then
        # Switch an existing host's service to this release's helpers,
        # keeping its tunnel, host key and running daemon session.
        upgrade=true
        shift
    elif [[ $1 == --friendly-name && $# -ge 2 && -n $2 ]]; then
        friendly_name=$2
        shift 2
    elif [[ $1 =~ ^[0-9]+\.[0-9]+\.[0-9]+$ && -z $version ]]; then
        version=$1
        shift
    else
        fail "$usage"
    fi
done
if [[ -n $manage ]]; then
    [[ $manage == list || $manage == delete || $manage == delete-all || $manage == archive ]] ||
        fail 'Use --manage list, --manage delete/archive UUID --yes, or --manage delete-all --yes.'
    if "$install_dependencies" || "$no_takeover" || "$upgrade" || "$repair_session" || [[ -n $friendly_name || -n $file_root || -n $restart_unexpected ]] ||
        "$file_write" || "$disable_files" || "$disable_observation" || ((${#observe_roots[@]})); then
        fail 'Management cannot be combined with setup/upgrade options.'
    fi
    if [[ $manage == list ]]; then
        ! "$yes" || fail '--yes applies only to deletion.'
    else
        "$yes" || fail 'Deletion requires explicit --yes confirmation.'
    fi
    if [[ $manage == delete || $manage == archive ]]; then
        [[ $installation_id =~ ^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$ ]] ||
            fail 'Delete requires an exact local installation UUID.'
    fi
    if [[ -n $confirm_legacy_cloud_id ]]; then
        [[ $manage == delete && ${#confirm_legacy_cloud_id} -le 256 &&
            $confirm_legacy_cloud_id =~ ^[a-zA-Z0-9.-]+$ ]] ||
            fail '--confirm-legacy-cloud-id is an exact qualified ID for --manage delete only.'
    fi
    management_arguments=(--manage "$manage")
    if [[ -n $installation_id ]]; then management_arguments+=("$installation_id"); fi
    if "$yes"; then management_arguments+=(--yes); fi
    if "$json"; then management_arguments+=(--json); fi
    if [[ -n $confirm_legacy_cloud_id ]]; then management_arguments+=(--confirm-legacy-cloud-id "$confirm_legacy_cloud_id"); fi
    [[ $(id -u) != 0 ]] || fail 'Run as your normal user, not root.'
    management_root="$HOME/.local/share/festerm-tunnels"
    # Execute only a previously installed private helper, never a download or a
    # JSON-supplied executable path. The native helper owns inventory authority.
    management_os=$(uname -s)
    trusted_management_path() {
        local path=$1 private=$2 component= part details owner mode
        [[ $path == /* ]] || fail 'Management helper path must be absolute.'
        local -a parts
        IFS='/' read -r -a parts <<< "$path"
        for part in "${parts[@]}"; do
            [[ -n $part ]] || continue
            [[ $part != . && $part != .. ]] || fail 'Management path is not normalized.'
            component="$component/$part"
            [[ ! -L $component ]] || fail 'Management path must not be a symbolic link.'
            if [[ $management_os == Darwin ]]; then details=$(stat -f '%u %Lp' "$component");
            else details=$(stat -c '%u %a' "$component"); fi
            read -r owner mode <<< "$details"
            [[ $owner == "$UID" || $owner == 0 ]] || fail 'Management path has a foreign owner.'
            (( (8#$mode & 0022) == 0 )) || fail 'Management path permits foreign writes.'
            if [[ -d $component ]]; then
                [[ -x $component ]] || fail 'Management path cannot be searched; inventory is unknown.'
            fi
        done
        if [[ $private == true ]]; then
            [[ $owner == "$UID" ]] && (( (8#$mode & 0077) == 0 )) ||
                fail 'Management cache must be private to the current user.'
        fi
    }
    management_parent=$management_root
    while [[ ! -e $management_parent && ! -L $management_parent ]]; do
        management_parent=${management_parent%/*}
        [[ -n $management_parent ]] || fail 'Management path is not absolute or verifiable.'
    done
    trusted_management_path "$management_parent" false
    if ! "$install_management_tools" && [[ ! -e $management_root && ! -L $management_root ]]; then
        [[ $manage != delete && $manage != archive ]] || fail 'No verified local installation.'
        if "$json"; then printf '%s\n' '{"version":1,"scope":"verified-local-script-installations","hosts":[]}';
        else printf 'No verified local script installations. Desktop hosts are managed in fesTerm.\n'; fi
        exit 0
    fi
    if "$install_management_tools"; then
        if [[ -e $management_root || -L $management_root ]]; then
            trusted_management_path "$management_root" true
        fi
        if [[ -e $management_root/releases || -L $management_root/releases ]]; then
            trusted_management_path "$management_root/releases" true
        fi
    else
    trusted_management_path "$management_root" true
    trusted_management_path "$management_root/releases" true
    management_helper=
    management_count=0
    for candidate in "$management_root"/releases/*; do
        name=${candidate##*/}
        [[ $name =~ ^[0-9]+\.[0-9]+\.[0-9]+-(linux|macos)-(x86_64|aarch64)$ ]] || continue
        if [[ -n $version && $name != "$version-"* ]]; then continue; fi
        management_count=$((management_count + 1))
        ((management_count <= 1000)) || fail 'Local helper inventory limit exceeded.'
        trusted_management_path "$candidate" true
        helper="$candidate/festerm-tunnel-setup"
        [[ -f $helper && -x $helper ]] || fail 'Cached management helper is missing.'
        trusted_management_path "$helper" true
        if [[ $management_os == Darwin ]]; then codesign --verify --strict "$helper"; fi
        help_text=$("$helper" --help) || fail 'Cached helper help failed.'
        if [[ $help_text == *'--manage {list'* ]]; then management_helper=$helper; fi
    done
    [[ -n $management_helper ]] ||
        fail 'No cached management helper. Rerun this command with --install-management-tools to stage verified tools only; no setup, login or service operation is required.'
    exec "$management_helper" "${management_arguments[@]}"
    fi
fi
if [[ -z $manage ]] && { "$yes" || "$json" || "$install_management_tools" || [[ -n $confirm_legacy_cloud_id ]]; }; then
    fail '--yes, --json, --confirm-legacy-cloud-id and --install-management-tools require --manage.'
fi
setup_extra=()
if "$repair_session"; then setup_extra+=(--repair-session); fi
if "$disable_files" && { [[ -n $file_root ]] || "$file_write"; }; then
    fail 'Use --file-root with optional --allow-file-write, or --disable-files.'
fi
if "$file_write" && [[ -z $file_root ]]; then fail '--allow-file-write requires --file-root.'; fi
if [[ -n $file_root ]]; then setup_extra+=(--file-root "$file_root"); fi
if "$file_write"; then setup_extra+=(--allow-file-write); fi
if "$disable_files"; then setup_extra+=(--disable-files); fi
if "$disable_observation" && ((${#observe_roots[@]})); then
    fail 'Use --observe-root directories, or --disable-observation.'
fi
for observe_root in ${observe_roots[@]+"${observe_roots[@]}"}; do setup_extra+=(--observe-root "$observe_root"); done
if "$disable_observation"; then setup_extra+=(--disable-observation); fi
if "$no_takeover"; then setup_extra+=(--no-takeover); fi
if "$upgrade"; then setup_extra+=(--upgrade); fi
if [[ -n $restart_unexpected ]]; then setup_extra+=("$restart_unexpected"); fi
if [[ -n $friendly_name ]]; then
    "$upgrade" && fail '--friendly-name applies only when creating a new host; rename an existing tunnel in fesTerm.'
    setup_extra+=(--friendly-name "$friendly_name")
fi
[[ $(id -u) != 0 ]] || fail 'Run as your normal user, not root.'
if [[ -z $version ]]; then
    # The newest published release; its redirect names the tag without the API.
    command -v curl >/dev/null || fail 'Missing curl; install the runtime prerequisites first.'
    latest=$(curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
        --connect-timeout 20 --max-time 60 --output /dev/null --write-out '%{url_effective}' \
        https://github.com/fes/fesTerm/releases/latest) || fail 'Could not look up the latest release.'
    version=${latest##*/v}
    [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'No published fesTerm release was found; pass VERSION explicitly.'
fi
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
if "$install_dependencies" && [[ $platform != linux ]]; then
    fail '--install-dependencies is Linux-only; macOS setup does not use sudo.'
fi
if [[ $platform == linux ]]; then cli_file_sha=$cli_sha; fi
if [[ $platform == linux ]]; then
    libc=$(getconf GNU_LIBC_VERSION) || fail 'glibc Linux is required; musl/Alpine is unsupported.'
    awk -v v="${libc#glibc }" 'BEGIN {split(v,a,"."); exit !(a[1]>2 || (a[1]==2 && a[2]>=39))}' ||
        fail 'The helper release requires glibc 2.39 or newer (Ubuntu 24.04+, Debian 13+).'
    if "$install_dependencies"; then
        command -v sudo >/dev/null || fail 'Dependency installation requires sudo; have an administrator install prerequisites, then rerun as your normal user.'
        printf 'Installing runtime packages with sudo; tunnel setup remains unelevated.\n'
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
    rm -f -- "$stage/bundle" "$stage/checksum" "$stage/cli" "$stage/expected" "$stage/actual" "$stage/types"
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
tar -tvzf "$stage/bundle" > "$stage/types" || fail 'Could not validate helper archive file types.'
if grep -qv '^-' "$stage/types"; then
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
if [[ -z $manage ]]; then "$install/devtunnel" --version; fi
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
if [[ -z $manage ]]; then
    printf 'Installed private helpers: %s\n' "$install"
    printf 'Setup reuses this user installation and its exact tunnel/session; a dedicated per-user service hosts it.\n'
fi
# Each release has its own cache directory; an upgrade leaves the previous
# release's helpers in place next to it.
cleanup
trap - EXIT
if [[ -n $manage ]]; then
    help_text=$("$install/festerm-tunnel-setup" --help) || fail 'Verified management helper help failed.'
    [[ $help_text == *'--manage {list'* ]] ||
        fail 'The verified release lacks management support; no setup, login or service operation was performed.'
    exec "$install/festerm-tunnel-setup" "${management_arguments[@]}"
fi
"$install/festerm-tunnel-setup" --managed --devtunnel "$install/devtunnel" --state-dir "$root/state" ${setup_extra[@]+"${setup_extra[@]}"}
profile="$root/state/festerm-profile.json"
if [[ ! -e $profile && ! -L $profile ]] && { "$disable_files" || "$disable_observation"; }; then
    printf 'Requested grant revocation saved; no connection JSON is available.\n'
    exit 0
fi
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
