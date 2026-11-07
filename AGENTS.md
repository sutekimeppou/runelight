# Runelight

Runelight is a macOS launcher written in Rust with Rune and Metal. It searches installed apps and configured quicklinks, ranks results using launch history, and opens them with macOS `open`. The default global shortcut is Option+Space.

## Code map

- `src/main.rs` owns launcher state, search ranking, keyboard handling, background refresh, and window lifecycle.
- `src/apps.rs` discovers app bundles. `src/config.rs` loads and validates quicklinks, fallbacks, and the shortcut.
- `src/history.rs` persists successful app and quicklink launches and scores prior queries. `src/complete.rs` handles fuzzy matching configuration and inline completion.
- `src/view.rs` draws the panel and animations. `src/glow.rs` implements the border light with a wgpu shader.

## Rune

- Rune's API changes between revisions. Consult the [README and agent tooling guide](https://github.com/anomalyco/rune/blob/396a9af15ed63a9aede6b60871668f7f4c0665db/crates/rune/README.md#agent-tooling-headless-no-window-same-renderer-fixed-virtual-clock) and [examples](https://github.com/anomalyco/rune/tree/396a9af15ed63a9aede6b60871668f7f4c0665db/examples) at the revision pinned in `Cargo.toml`. The same source is available under `~/.cargo/git/checkouts/rune-*/`.
- Rune can render headless PNGs with `--snap`, dump layout and accessibility data with `--tree`, replay JSON input with `--script`, and record animation with `--trace` or `--frames`. These use a virtual clock and the renderer used by the app.
- Runelight initializes and shows its panel in `on_ready`, a native-window callback that headless runs skip. A useful headless launcher snapshot needs an explicit test state or scene; native window behavior still needs separate verification. Runelight also reads local config and history, so isolate `XDG_CONFIG_HOME` and `XDG_STATE_HOME` when using fixtures.

## Development

- Use macOS and Rust stable. Rune is pinned to a Git revision in `Cargo.toml`; its Metal backend and macOS APIs make this a platform-specific project.
- Run `cargo run` to start the launcher, `cargo fmt --check` to check formatting, and `cargo check` to check the build.
- `.github/workflows/release.yml` builds the Apple Silicon app bundle and publishes a Homebrew cask when a `v<version>` tag matches `Cargo.toml`. Bundle metadata and the cask template are in `packaging/`.
