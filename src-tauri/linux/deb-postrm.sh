#!/bin/sh
# Removes the folder entry this app writes into each user's own applications
# directory, when the package is removed.
#
# The package never installed that file — the app writes it from its own
# settings — so the package manager does not know about it and would leave it
# behind.
#
# The first argument says why this script is running; only a real removal should
# clean up, never an upgrade.
DESKTOP_FILE="com.bulentgercek.bgbucketbrowser-openhere.desktop"

case "$1" in
  remove|purge)
    getent passwd | awk -F: '($3 >= 1000 && $3 < 60000) || $3 == 0 {print $6}' | sort -u | while read -r home_dir; do
      target="$home_dir/.local/share/applications/$DESKTOP_FILE"
      if [ -f "$target" ]; then
        rm -f "$target"
        if command -v update-desktop-database >/dev/null 2>&1; then
          update-desktop-database "$home_dir/.local/share/applications" >/dev/null 2>&1 || true
        fi
      fi
    done
    ;;
esac

exit 0
