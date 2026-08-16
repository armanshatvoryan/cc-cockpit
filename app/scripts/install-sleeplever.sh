#!/bin/bash
# One-time root setup for the CC Cockpit lid-proof sleep lever.
# Installs the root helper + a NOPASSWD sudoers entry scoped to it.
# Run: sudo bash install-sleeplever.sh
#
# Security shape: the helper lands root-owned/root-writable-only at a fixed
# path, and the sudoers grant names that exact path — so the passwordless
# grant covers only the pmset toggling above, nothing else.
set -euo pipefail

[[ $EUID -eq 0 ]] || { echo "run with sudo" >&2; exit 1; }
# sudo sets SUDO_USER; osascript-with-admin-privileges runs straight as root,
# so fall back to whoever owns the console (the logged-in GUI user).
user="${SUDO_USER:-$(stat -f%Su /dev/console)}"
[[ -n "$user" && "$user" != "root" ]] || { echo "cannot determine invoking user" >&2; exit 1; }

src_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
helper=/usr/local/bin/cc-cockpit-sleeplever

install -o root -g wheel -m 0755 "$src_dir/cc-cockpit-sleeplever" "$helper"

sudoers=/etc/sudoers.d/cc-cockpit-sleeplever
tmp="$(mktemp)"
printf '%s ALL=(root) NOPASSWD: %s\n' "$user" "$helper" > "$tmp"
chmod 0440 "$tmp"
visudo -c -f "$tmp" >/dev/null
mv "$tmp" "$sudoers"

echo "installed: $helper"
echo "sudoers:   $sudoers (NOPASSWD for $user, this helper only)"
