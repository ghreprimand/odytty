local wezterm = require("wezterm")

return {
  font = wezterm.font("DejaVu Sans Mono"),
  font_size = 12.0,
  harfbuzz_features = { "liga=0", "clig=0", "calt=0" },
  initial_cols = 80,
  initial_rows = 24,
  window_padding = { left = 0, right = 0, top = 0, bottom = 0 },
  colors = { foreground = "#c0c0c0", background = "#101010" },
  window_background_opacity = 1.0,
  text_background_opacity = 1.0,
  cursor_blink_rate = 0,
  audible_bell = "Disabled",
  scrollback_lines = 100000,
  enable_tab_bar = false,
  animation_fps = 1,
}
