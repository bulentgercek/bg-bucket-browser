#!/bin/bash
# Signs the development binary with one fixed identity before running it.
#
# Each rebuild is otherwise signed ad hoc with a different identity, and the
# keychain ties its permissions to that: every rebuild counts as a different
# application and asks for access again. With a stable identity, allowing it once
# is enough.
#
# The identity is a self-signed certificate that exists only on the machine that
# created it, so on any other machine the app still has to start: signing is a
# convenience here, not a requirement, and a missing identity is a warning.
set -e
IDENTITY="${BG_CODESIGN_IDENTITY:-bg-bucket-browser-dev}"
BIN="$1"
shift
if ! codesign --force --deep --sign "$IDENTITY" "$BIN" 2>/dev/null; then
  echo "codesign-runner: signing identity '$IDENTITY' not found; starting unsigned." >&2
  echo "codesign-runner: the keychain will ask for access again after each rebuild." >&2
fi
exec "$BIN" "$@"
