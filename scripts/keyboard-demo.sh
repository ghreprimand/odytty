#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# keyboard-demo.sh — verify the keyboard protocols by eye (docs/features.md).
#
# Three stages:
#   1. Query the terminal: the Kitty keyboard flags (`CSI ? u`) and the
#      xterm modifyOtherKeys level (`XTQMODKEYS`, `CSI ? 4 m`), printed as
#      received.
#   2. Kitty disambiguate: push flag 1 and echo the raw bytes of every key
#      pressed, so Shift+Enter (`CSI 13;2u`), Ctrl+Enter (`CSI 13;5u`),
#      Ctrl+Backspace (`CSI 127;5u`), and the F-key forms can be eyeballed.
#   3. modifyOtherKeys level 2: pop the Kitty flag (non-zero Kitty flags
#      take precedence, so it must be off), set mok2, and echo again — the
#      same chords now arrive as `CSI 27 ; modifier ; codepoint ~`.
#
# All protocol state is restored on every exit path (including Ctrl+C and
# SIGTERM): the Kitty flag is popped, the mok level is reset with a bare
# `CSI > m`, and the tty settings are put back.
#
# The script is self-contained raw sequences and is safe in any terminal: one
# that supports neither protocol answers no queries (stage 1 times out) and
# keeps sending legacy bytes in stages 2 and 3.
#
# Windows: Unix shells only. In a native Windows (ConPTY) session, conhost's
# input converter normalizes enhanced key encodings before applications see
# them, so stages 2 and 3 would show legacy bytes regardless of the
# terminal's support — run the demo in a WSL session instead, whose input
# path bypasses the converter (see docs/features.md).
#
# Usage:  bash scripts/keyboard-demo.sh
set -u

esc=$(printf '\033')

# --- restore-on-exit ----------------------------------------------------------
stty_saved=$(stty -g 2>/dev/null) || stty_saved=""
kitty_pushed=0
mok_set=0
cleanup() {
  [ "$kitty_pushed" -eq 1 ] && printf '\e[<u'  # pop the pushed Kitty flags
  [ "$mok_set" -eq 1 ] && printf '\e[>m'       # reset modifyOtherKeys to 0
  [ -n "$stty_saved" ] && stty "$stty_saved" 2>/dev/null
  printf '\n'
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo
echo "OdyTTY keyboard protocol demo"
echo "============================="
[ -n "$stty_saved" ] && stty -echo -icanon 2>/dev/null

# --- stage 1: queries ---------------------------------------------------------
# Send both queries, then drain replies for a bounded window. Replies:
#   Kitty flags:  CSI ? <flags> u
#   XTQMODKEYS:   CSI > 4 ; <level> m
echo
echo "Stage 1: protocol queries"
printf '\e[?u\e[?4m'
reply=""
deadline=$((SECONDS + 2))
while [ "$SECONDS" -lt "$deadline" ]; do
  ch=""
  IFS= read -rsn1 -t 1 ch || continue
  reply="$reply$ch"
done
kitty_flags="(no reply)"
mok_level="(no reply)"
case "$reply" in
  *"$esc"'[?'*u*)
    kitty_flags="${reply##*"$esc"'[?'}"
    kitty_flags="flags ${kitty_flags%%u*}"
    ;;
esac
case "$reply" in
  *"$esc"'[>4;'*m*)
    mok_level="${reply##*"$esc"'[>4;'}"
    mok_level="level ${mok_level%%m*}"
    ;;
esac
echo "  Kitty keyboard protocol: $kitty_flags"
echo "  modifyOtherKeys (XTQMODKEYS): $mok_level"

# --- key echo loop ------------------------------------------------------------
# Read bytes and print each burst as hex + a readable rendering. A pause in
# input ends the burst, so every keypress prints on its own line. A lone 'q'
# byte (0x71) ends the stage.
echo_keys() {
  buf=""
  while :; do
    ch=""
    if IFS= read -rsn1 -t 1 ch; then
      # `read` returns an empty string for both a NUL byte and a newline;
      # either way keep a marker so the burst is visibly non-empty.
      if [ -z "$ch" ]; then
        buf="$buf<0>"
      else
        buf="$buf$ch"
      fi
      continue
    fi
    [ -z "$buf" ] && continue
    [ "$buf" = "q" ] && return 0
    hexed=$(printf '%s' "$buf" | od -An -tx1 | tr -d '\n' | tr -s ' ')
    shown=${buf//"$esc"/^[}
    printf '  hex%s  |%s|\n' "$hexed" "$shown"
    buf=""
  done
}

# --- stage 2: Kitty disambiguate ---------------------------------------------
echo
echo "Stage 2: Kitty disambiguate (flag 1 pushed)"
echo "  Try Shift+Enter, Ctrl+Enter, Ctrl+Backspace, F1-F12, Ctrl+I vs Tab."
echo "  Expect CSI-u forms like 1b 5b 31 33 3b 32 75 (= ^[[13;2u, Shift+Enter)."
echo "  Press q (then pause) to move on."
printf '\e[>1u'
kitty_pushed=1
echo_keys
printf '\e[<u'
kitty_pushed=0

# --- stage 3: modifyOtherKeys level 2 ----------------------------------------
echo
echo "Stage 3: modifyOtherKeys level 2 (Kitty flags popped first)"
echo "  The same chords now arrive as CSI 27;<modifier>;<codepoint>~ forms,"
echo "  for example 1b 5b 32 37 3b 35 3b 31 33 7e (= ^[[27;5;13~, Ctrl+Enter)."
echo "  Press q (then pause) to finish."
printf '\e[>4;2m'
mok_set=1
echo_keys
printf '\e[>m'
mok_set=0

echo
echo "Done. Protocol state restored."
