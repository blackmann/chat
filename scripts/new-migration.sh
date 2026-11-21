#!/bin/sh
# Make an empty migration named for the time now (UTC) and what it does:
#   scripts/new-migration.sh pin_threads
#   → migrations/20261121143005_pin_threads.sql
set -eu
cd "$(dirname "$0")/.."
name="${1:?usage: scripts/new-migration.sh what_it_does}"
case "$name" in
*[!a-z0-9_]*) echo "use lowercase letters, digits and underscores: $name" >&2; exit 1 ;;
esac
file="migrations/$(date -u +%Y%m%d%H%M%S)_$name.sql"
printf -- '-- %s\n' "$name" > "$file"
echo "$file"
