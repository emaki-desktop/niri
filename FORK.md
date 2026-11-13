# Fork notes — niri

This is a fork of [niri](https://github.com/niri-wm/niri), the scrollable-tiling Wayland
compositor, for the [emaki](https://github.com/emaki-desktop/emaki) desktop. It is a
thin series of commits on top of a tagged upstream release; everything it doesn't name below is
stock niri.

## What it adds

- **Materials:** named glass looks drawn over the blur, set per window or layer rule.
- **Blur shapes:** the `niri-blur-shapes-v1` protocol, rounded fading blur shapes with a material and tint.
- **Vibrancy:** the `niri-vibrancy-v1` protocol, a subsurface blended with its backdrop.
- **Effects memory:** blur chains, xray buffers and captures built when used and freed when idle.
- **Secure fill:** the `niri-secure-fill-v1` protocol, a password manager fills a field only if it's still the one it checked.
- **Desktop layer:** the `desktop` layer rule, a layer that stays still on the wallpaper, and the `DesktopObscured` IPC event.
- **Workspace mat:** a backing behind the scrolling layout that fills the gaps.
- **Gaps:** gaps between workspaces, and between windows in the overview.
- **Mod swipe:** Mod-held three-finger swipes that close, maximize and resize the focused window.
- **IPC events:** per-frame gesture progress (`OverviewProgress`, `ViewOffsetChanged`, `WorkspaceSwitchProgress`), `Window.is_fullscreen`, `KeyboardLockKeysChanged`, `OutputsChanged`.
- **IPC actions:** `MoveColumnToIndex` by window id, so a dock moves a column without moving the focus.
- **Features:** the `Features` request, which of these the running niri has; stock niri answers with an error.
- **Wallpaper dim:** the `SetWallpaperDim` IPC action, the wallpaper dimmed or not drawn over a black backdrop, for OLED panels; see [power.md](docs/fork/display/power.md).
- **Config watcher:** the config and its includes watched with inotify instead of polled twice a second; see [power.md](docs/fork/display/power.md#config-watcher).
- **Power:** glass damaged only where its backdrop changed, scissored re-blurs, no redraws for commits nothing shows, `suspended` for windows on hidden workspaces, a closed lid turning the panel off, on-demand timers; see [power.md](docs/fork/display/power.md#damage-redraws-and-wakeups-the-fork-cuts).
- **HDR:** HDR10 output (`hdr`, `hdr-auto`, `sdr-brightness`) with the SDR desktop unchanged, and color-managed HDR surfaces through `wp-color-management-v1`.

## Branch and base

- Branch `emaki`, based on upstream `v26.04`; the fork point is tagged `fork-base-v26.04`.
- The delta: `git log --oneline fork-base-v26.04..emaki`.

## Using it

Running the fork, building it into a NixOS config, each feature's config, protocol or IPC
surface, and what emaki relies on:
**[docs/fork/README.md § Using the fork](docs/fork/README.md#using-the-fork)**.

## Maintaining it

Branches, tags, the rebase routine onto the next upstream release, the hot upstream files and
the commit log: **[docs/fork/maintaining.md](docs/fork/maintaining.md)**, introduced in
[docs/fork/README.md § Maintaining the fork](docs/fork/README.md#maintaining-the-fork).

## Footprint on upstream's files

Upstream's files, the wiki included, are unchanged from `v26.04`, with two exceptions: the
one-line banner at the top of `README.md`, and `Cargo.toml`, where `libdisplay-info` gains the
`v0_2` feature that HDR needs (`.gitignore` also ignores `.claude/`). The fork's logic lives in
its own source files, with short hooks in upstream's. New files: the protocols
`resources/niri-blur-shapes-v1.xml`, `resources/niri-vibrancy-v1.xml` and
`resources/niri-secure-fill-v1.xml`, the fork's CI `.github/workflows/fork.yml`, this file, and
all fork docs under [`docs/fork/`](docs/fork/README.md).
