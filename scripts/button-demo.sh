#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# button-demo.sh — demonstrate the OdyTTY button protocol (docs/buttons.md).
#
# Prints a small panel of clickable buttons using the Tier 2 spelling
# (OSC 133;P;odytty-button), plus one iTerm2-compatible Tier 1 button, then
# waits for click reports (CSI ? 1337 ; code ~) on stdin and reacts to them.
#
# The script is self-contained: it emits raw sequences and does not depend on
# the shell-integration helpers. In a terminal without button support the
# labels print as plain text, no report ever arrives, and the demo exits on
# its timeout. Buttons are default-off in OdyTTY (the `buttons` master gate);
# with the gate off, OdyTTY behaves like any other terminal here.
#
# Usage:  bash scripts/button-demo.sh
set -u

# --- emitters (mirror the shell-integration helpers) -------------------------
btn() { # CODE LABEL [ICON] [SCOPE]
  printf '\e]133;P;odytty-button;code=%s%s%s\a%s\e]133;P;odytty-button;end\a' \
    "$1" "${3:+;icon=$3}" "${4:+;scope=$4}" "$2"
}
btn_clear() { # [CODE]
  printf '\e]133;P;odytty-button;invalidate%s\a' "${1:+;code=$1}"
}

# --- the panel ----------------------------------------------------------------
echo
echo "OdyTTY button protocol demo"
echo "==========================="
echo
# Feature discovery (docs/buttons.md): OdyTTY sets ODYTTY_BUTTONS=1 in the
# session environment when its buttons setting is on. The demo still prints
# the panel either way (the sequences are safe everywhere); this is a heads-up.
if [ -z "${ODYTTY_BUTTONS-}" ]; then
  echo "  note: ODYTTY_BUTTONS is not set, so buttons are off or unsupported"
  echo "  in this terminal. The labels below will print as plain text."
  echo
fi
printf '  '; btn 1 '[ Run build ]' run
printf '   '; btn 2 '[ Run tests ]' check
printf '   '; btn 3 '[ Copy log path ]' copy
printf '\n\n'
printf '  sticky (survives the next prompt): '
btn 4 '[ Retry last deploy ]' retry sticky
printf '\n\n'
# Tier 1 compat spelling (iTerm2): a point button, no label run. It renders
# as a chip at the end of this line's content; other terminals drop the OSC
# and show only the text.
printf '\e]1337;Button=type=custom;code=5;icon=star\a  iTerm2-spelled point button on this line\n'
printf '\n  '; btn 9 '[ Quit demo ]' stop
printf '\n\n'
echo "Click a button, press q, or wait 30s to quit."
echo "(No reaction below means buttons are off or unsupported here.)"
echo

# --- read click reports: ESC [ ? 1337 ; <code> ~ ------------------------------
stty_saved=$(stty -g 2>/dev/null) || stty_saved=""
cleanup() {
  [ -n "$stty_saved" ] && stty "$stty_saved" 2>/dev/null
  btn_clear
  printf '\n'
}
trap cleanup EXIT
[ -n "$stty_saved" ] && stty -echo -icanon 2>/dev/null

handle() {
  case "$1" in
    1) echo "  -> build clicked: pretending to build... done." ;;
    2) echo "  -> tests clicked: pretending to test... 0 failed." ;;
    3) echo "  -> copy clicked: pretending the log path is on the clipboard." ;;
    4) echo "  -> sticky retry clicked: this one outlives prompts." ;;
    5) echo "  -> the iTerm2-spelled button works too." ;;
    9) echo "  -> quit clicked."; exit 0 ;;
    *) echo "  -> button code $1 clicked." ;;
  esac
}

deadline=$((SECONDS + 30))
buf=""
esc=$(printf '\033')
while [ "$SECONDS" -lt "$deadline" ]; do
  ch=""
  IFS= read -rsn1 -t 1 ch || continue
  case "$ch" in
    q|Q) exit 0 ;;
  esac
  buf="$buf$ch"
  case "$buf" in
    *"$esc"'[?1337;'*'~')
      code="${buf##*'[?1337;'}"
      code="${code%'~'}"
      case "$code" in
        ''|*[!0-9]*) ;; # not a click report; drop it
        *) handle "$code" ;;
      esac
      buf=""
      ;;
    *"$esc"*) : ;; # partial escape sequence: keep accumulating
    *) buf="" ;;   # plain keys: discard
  esac
done
echo "  (demo timed out)"
