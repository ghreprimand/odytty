# OdyTTY Runtime Knobs

OdyTTY is still a prototype, so runtime configuration currently comes from
environment variables loaded once at native startup. Defaults are chosen to keep
the no-env launch path stable:

```sh
cargo run -- --native
```

## Native Settings

| Variable | Values | Default | Notes |
| --- | --- | --- | --- |
| `ODYTTY_FONT_SIZE` | Pixel size, clamped to `6.0..=72.0` | `14.0` | Controls native glyph rasterization, cell size, initial window size, and resize grid fitting. Invalid values fall back to `14.0` with one stderr warning. |
| `ODYTTY_FONT` | Path to a `.ttf` or `.otf` font file | Host monospace probe list | Overrides the probed Linux monospace font. |
| `ODYTTY_THEME` | `plain`, `odyssey`, `odyssey-noir` | `plain` | Selects default foreground/background and window clear color. Unknown values fall back to `plain`. |
| `ODYTTY_VISUAL` | `off`, `none`, `plain`, `ambient`, `scanlines` | `off` | Enables or disables the optional presentation-only ambient effect. |
| `ODYTTY_NATIVE_AUTOCLOSE_MS` | Positive integer milliseconds | unset | Development/smoke-test helper that closes the native window after the delay. `0`, unset, or invalid values disable autoclose. |

## Examples

Run with larger text:

```sh
ODYTTY_FONT_SIZE=18 cargo run -- --native
```

Run with an Odyssey theme and ambient visual treatment:

```sh
ODYTTY_THEME=odyssey ODYTTY_VISUAL=ambient cargo run -- --native
```

Run with an explicit font:

```sh
ODYTTY_FONT=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf cargo run -- --native
```

Run a non-interactive native lifecycle smoke check:

```sh
ODYTTY_NATIVE_AUTOCLOSE_MS=600 cargo run -- --native
```
