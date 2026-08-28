# CSA lightweight TUI harness

This standalone Ratatui program displays every Subagent Live lifecycle state,
the transparent compact-square-point Orbit, and an automatic normal-flow loop:
Starting → Running with three sequential activities → Done. It does not simulate
the surrounding Codex CLI shell, compile Codex, or start agents, models, tools,
authentication, configuration, or network requests.

```powershell
cargo run --manifest-path tests/ui/Cargo.toml
```

The first build may download Ratatui/Crossterm. Later runs reuse the small local
Cargo target. Windows Terminal defaults to Sixel; recognized Kitty-compatible
terminals default to Kitty graphics. Press `g` to cycle the local override
through text, Sixel, and Kitty. A forced protocol only renders in a terminal
that supports it. Kitty locks only the one-column width so the terminal preserves
the same raster aspect ratio used by Sixel, while spreading adjacent Orbit
squares by one additional physical pixel.

Controls:

- `m`: animated/reduced motion
- `g`: text/Sixel/Kitty mode
- `q` or `Esc`: quit

Run the focused check with:

```powershell
cargo test --manifest-path tests/ui/Cargo.toml
```

This is fast visual feedback only. It is not full Codex, terminal-protocol,
ConPTY, patched-binary, or release acceptance evidence. The fallback raster is
10×20 pixels when the terminal does not report cell pixel dimensions.
