#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-only
# make-demo.sh — reproduce the OdyTTY README splash for a fresh screenshot.
#
# Renders the showcase frame inside a REAL OdyTTY window (so it uses OdyTTY's
# own glyph atlas + CRT/bloom/vignette post-processing), then helps you capture
# it with grim+slurp. Everything runs from a sanitized /tmp fake repo — no real
# user data, safe for a public screenshot.
#
# Tagline is "a GPU-accelerated terminal emulator written in Rust" — no version,
# no "for Linux" (the only change from the original v0.2.0 showcase).
#
# Usage:  bash scripts/make-demo.sh  [output.png]
#         (default output: ~/odytty-demo-new.png)
set -u

DEMO=/tmp/odytty-demo
OUT="${1:-$HOME/odytty-demo-new.png}"

# --- pick an odytty binary: repo release build, else newest local dev build ---
BIN=""
if [ -x target/release/odytty ]; then BIN="$(pwd)/target/release/odytty"; fi
if [ -z "$BIN" ]; then
  BIN="$(ls -dt "$HOME"/.local/opt/odytty/dev-*/bin/odytty 2>/dev/null | head -n1)"
fi
if [ -z "$BIN" ] || [ ! -x "$BIN" ]; then
  echo "ERROR: no odytty binary found. Build target/release/odytty first." >&2
  exit 1
fi
echo "using binary : $BIN"
echo "output target: $OUT"

# --- build a sanitized fake repo so 'git log --graph' + 'tree' have content ---
rm -rf "$DEMO"; mkdir -p "$DEMO/src" "$DEMO/shaders"; cd "$DEMO"
cat > README.md <<'EOF'
# OdyTTY
A from-scratch, GPU-rendered terminal emulator.
EOF
cat > Cargo.toml <<'EOF'
[package]
name = "odytty"
edition = "2021"
EOF
git init -q
git config user.name "odytty"; git config user.email "dev@example.com"
commit() { export GIT_AUTHOR_DATE="$1" GIT_COMMITTER_DATE="$1"; git add -A && git commit -qm "$2"; }

printf 'pub fn parse(_b: &[u8]) {}\n' > src/parser.rs
commit "2024-02-01T10:00:00" "Owned DEC/xterm escape parser"
printf 'pub struct Atlas;\n' > src/atlas.rs
commit "2024-02-04T11:00:00" "GPU glyph atlas with HiDPI rebuilds"
git branch -q inline-graphics
printf 'pub fn alt_scroll() {}\n' > src/mouse.rs
commit "2024-02-06T09:30:00" "Implement alternate scroll mode (DECSET 1007)"
git checkout -q inline-graphics
printf '// kitty + sixel transports\n' > src/graphics.rs
commit "2024-02-06T15:00:00" "Sixel + Kitty inline graphics transports"
git checkout -q master
export GIT_AUTHOR_DATE="2024-02-07T16:00:00" GIT_COMMITTER_DATE="2024-02-07T16:00:00"
git merge -q --no-ff inline-graphics -m "Merge inline-graphics: Sixel + Kitty"
printf 'pub const DEFAULT_SCROLLBACK: usize = 10_000;\n' > src/scrollback.rs
commit "2024-02-09T12:00:00" "Bounded scrollback + deterministic test harness"
printf '// bloom + CRT scanline post-processing\n@fragment fn fs() {}\n' > shaders/post.wgsl
commit "2024-02-09T12:05:00" "shaders: bloom + CRT post-processing pass"

# --- showcase frame (tagline: no version, no "for Linux") ---
cat > /tmp/odytty-showcase.sh <<'SHEOF'
#!/usr/bin/env bash
# Sanitized OdyTTY showcase frame. No real user data — safe for screenshots.
set -u
cd /tmp/odytty-demo 2>/dev/null || true

clear
printf '\n'

# --- Banner: block-art "OdyTTY" in a magenta->cyan truecolor gradient ---
# Kerned standard-font glyphs, composed programmatically so every column
# aligns (the previous hand-typed art fused the second T with the Y and
# dropped the T's right rail — visibly mangled in the README hero shot).
banner=(
'  ___       _         _____  _____ __   __'
' / _ \   __| | _   _ |_   _||_   _|\ \ / /'
'| | | | / _` || | | |  | |    | |   \ V / '
'| |_| || (_| || |_| |  | |    | |    | |  '
' \___/  \__,_| \__, |  |_|    |_|    |_|  '
'               |___/                      '
)
i=0
for line in "${banner[@]}"; do
  r=$((255 - i*14)); g=$((90 + i*26)); b=$((230 - i*4))
  printf '\033[38;2;%d;%d;%dm%s\033[0m\n' "$r" "$g" "$b" "$line"
  i=$((i+1))
done
printf '   \033[38;5;245ma GPU-accelerated terminal emulator written in Rust\033[0m\n\n'

# --- Truecolor gradient bar (shows 24-bit color + bloom on bright cells) ---
printf '  '
for x in $(seq 0 59); do
  r=$(( (x*255)/59 )); g=$(( 128 + (x*60)/59 )); b=$(( 255 - (x*200)/59 ))
  printf '\033[48;2;%d;%d;%dm \033[0m' "$r" "$g" "$b"
done
printf '\n\n'

# --- git graph ---
printf '\033[1;38;5;213m  \xe2\x9d\xaf git log --graph --oneline --all\033[0m\n'
git --no-pager log --oneline --graph --all --color=always 2>/dev/null | sed 's/^/  /' | head -n 9
printf '\n'

# --- project tree ---
printf '\033[1;38;5;81m  \xe2\x9d\xaf tree -L 2\033[0m\n'
if command -v tree >/dev/null 2>&1; then
  tree -L 2 -C --noreport 2>/dev/null | sed 's/^/  /' | head -n 12
fi
printf '\n'

# --- Style sampler: bold / italic / underline / strike / reverse + color ramp ---
printf '  \033[1mbold\033[0m  \033[3mitalic\033[0m  \033[4munderline\033[0m  \033[9mstrike\033[0m  \033[7mreverse\033[0m   '
for c in 196 208 226 46 51 21 201; do printf '\033[38;5;%dm\xe2\x97\x8f\033[0m ' "$c"; done
printf '\n'
printf '  \033[38;5;245mligatures:\033[0m \033[38;5;159m-> => != >= <= === |> :: <$> ++\033[0m\n\n'

# --- Fake prompt left on screen so the shot ends on a clean prompt ---
printf '\033[38;5;213mody\033[0m \033[38;5;245m~/projects/nebula\033[0m \033[38;5;81m\xe2\x9d\xaf\033[0m '
SHEOF
chmod +x /tmp/odytty-showcase.sh

# frame = showcase without the trailing fake prompt (PS1 provides the live one)
sed '/213mody/d' /tmp/odytty-showcase.sh > /tmp/odytty-showcase-frame.sh
chmod +x /tmp/odytty-showcase-frame.sh

cat > /tmp/odytty-rc.sh <<'RCEOF'
# Sanitized interactive shell for screenshots — no real user data.
export HISTFILE=/dev/null
unset HISTSIZE
PROMPT_COMMAND=
PS1=$'\033[38;5;213mody\033[0m \033[38;5;245m~/projects/nebula\033[0m \033[38;5;81m\xe2\x9d\xaf\033[0m '
cd /tmp/odytty-demo 2>/dev/null || true
# paint the showcase frame, then drop to the live prompt
bash /tmp/odytty-showcase-frame.sh
RCEOF

# --- launch OdyTTY showing the splash (uses your odytty.conf theme + effects) ---
echo "launching OdyTTY…"
cd "$DEMO"
setsid "$BIN" \
  --title "OdyTTY" \
  --working-directory "$DEMO" \
  -e env -i \
      HOME="$DEMO" \
      TERM=xterm-256color \
      PATH=/usr/local/bin:/usr/bin:/bin \
      SHELL=/bin/bash \
      LANG=en_US.UTF-8 \
      bash --noprofile --rcfile /tmp/odytty-rc.sh -i \
  >/tmp/odytty-demo.log 2>&1 &
sleep 3
if ! pgrep -af 'odytty' | grep -q "$BIN"; then
  echo "WARNING: OdyTTY may not have started — check /tmp/odytty-demo.log" >&2
fi

# --- capture ---
echo
echo "OdyTTY should now be showing the splash."
echo "Resize the window if you want more/less margin around the content."
echo
if command -v grim >/dev/null 2>&1 && command -v slurp >/dev/null 2>&1; then
  echo ">>> Press Enter, then DRAG-SELECT the OdyTTY terminal area to save the shot."
  read -r _
  if grim -g "$(slurp)" "$OUT"; then
    sz="?"; command -v identify >/dev/null 2>&1 && sz="$(identify -format '%wx%h' "$OUT" 2>/dev/null || echo '?')"
    echo "saved: $OUT  ($sz)"
  else
    echo "capture cancelled/failed." >&2
  fi
else
  echo "grim/slurp not found. Capture the window with your screenshot tool, e.g.:"
  echo "  grim -g \"\$(slurp)\" $OUT"
fi
