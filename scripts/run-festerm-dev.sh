#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
sandbox_root="$repository_root/target/festerm-dev"

mkdir -p "$sandbox_root/state"
export FESTERM_CONFIG_PATH="$sandbox_root/config.toml"
export XDG_STATE_HOME="$sandbox_root/state"

cd "$repository_root"
cargo build -p festerm-sessiond
exec cargo run -p festerm -- "$@"
