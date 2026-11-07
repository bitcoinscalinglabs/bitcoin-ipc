#!/bin/bash
# Writes the ipc-cli config of a validator for subnet B: `subnet-b.config.toml` next to
# `config.toml`, identical except that the Bitcoin parent entry points at the validator's
# subnet B provider (port 3130 + n - 1) instead of its subnet A provider (port 3030 + n - 1).
# ipc-cli takes the provider of the parent entry, shared by both subnets, so a join to subnet B
# with `--config-path .../config.toml` would reach the subnet A provider and its EOTS key.
# Derived from `config.toml` on every call: run again after appending entries to it.
#
# Usage: subnet_b_config.sh <n> [ipc-dir]    (ipc-dir defaults to /root/.ipc)
set -e

n="$1"
ipc_dir="${2:-/root/.ipc}"
if [ -z "$n" ] || ! [ "$n" -ge 1 ] 2>/dev/null; then
    echo "Usage: $0 <validator number> [ipc-dir]" >&2
    exit 1
fi

dir="${ipc_dir}/validator${n}"
src="${dir}/config.toml"
dst="${dir}/subnet-b.config.toml"
port_a=$((3029 + n))
port_b=$((3129 + n))

if [ ! -f "$src" ]; then
    echo "Error: ${src} not found" >&2
    exit 1
fi
if ! grep -q "^provider_http = \".*:${port_a}/api\"" "$src"; then
    echo "Error: ${src} has no Bitcoin provider entry on port ${port_a}" >&2
    exit 1
fi

sed -e "s|^\(provider_http = \".*\):${port_a}/api\"|\1:${port_b}/api\"|" "$src" > "${dst}.tmp"
mv "${dst}.tmp" "$dst"
