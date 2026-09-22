#!/bin/sh
# Runs the development build, leaving the terminal and the exit code clean.
# Invoked as ./dev from the project root.
#
# A POSIX shell script, so Linux and macOS only. The problems below were seen on
# one development machine's shell and terminal; elsewhere this is simply a
# wrapper around the same command, and running that command directly is fine.
#
# Detached input: the CLI probes the terminal for its colours and cursor
#   position as it starts, and the replies end up printed as escape sequences.
#   Without a terminal to see, it does not ask.
# Restoring the terminal mode: in case something garbles it anyway. Deliberately
#   not on an exit trap, which would overwrite the exit code in some shells.
# Masking the exit code: closing the window makes the CLI exit unpredictably
#   during teardown, while a real build failure has its own distinct code.

TTY=/dev/tty
SAVED=$(stty -g <"$TTY" 2>/dev/null)
restore() {
  [ -n "$SAVED" ] && stty "$SAVED" <"$TTY" 2>/dev/null
  return 0
}

trap 'restore; exit 130' INT
trap 'restore; exit 143' TERM

cargo tauri dev </dev/null
status=$?
restore

case "$status" in
  0 | 1) exit 0 ;;
  *) exit "$status" ;;
esac
