# Spec: desktop layers

Status: implemented in `12509ec6` · Target: niri fork + emaki's desktop widgets · Scope: a
layer rule that puts a background layer on the backdrop as the desktop, and an IPC event that tells
it when it can't be seen.

## Usage

### Layer rule: `desktop`

Set to `true` to make the surface the desktop: a layer in the backdrop, like
[`place-within-backdrop`](../../wiki/Configuration:-Layer-Rules.md#place-within-backdrop), that stays put while workspaces switch and
doesn't shrink in the overview, but sits above the other backdrop layers (the wallpaper), takes
input wherever no window is while the overview is closed, and fades out as the overview opens, in
step with the gesture, along with its background effect. Its popups (menus) show above the
windows.

As with the backdrop, the surface only shows through workspaces with a transparent
`background-color` (in `layout`), and only *background* layer surfaces that ignore exclusive zones
can be one.

The [`DesktopObscured`](../ipc/events.md#desktopobscured) event on the IPC event stream tells a desktop layer when nothing
under the workspaces shows on its output, so it can stop drawing.

```kdl
// Desktop widgets on the wallpaper.
layout {
    background-color "transparent"
}

layer-rule {
    match namespace="^amane\\.widgets\\."

    desktop true
}
```

## Design / internals

### 1. Goal

emaki's desktop widgets are a clear Background-layer surface with frosted cards on it. Drawn
as a stock background layer, niri copies it into every workspace (`scale_relocate_crop`): the cards
slide with a workspace switch, shrink with the overview and show on every workspace in it. They
should behave like things on the wallpaper:

- still, as the wallpaper is, while workspaces slide over them;
- the same size in the overview, fading out as it opens (with the gesture) and in as it closes;
- taking input wherever no window is, as a background layer does;
- able to stop drawing while nothing under the workspaces shows.

The wallpaper itself already does the first part, from `place-within-backdrop` with a transparent
`layout { background-color }`.

### 2. The `desktop` layer rule

`layer-rule { desktop true }`, resolved into `ResolvedLayerRules::desktop`. It applies only where
`place-within-backdrop` would (a Background layer with `exclusive_zone == DontCare`):
`MappedLayer::is_desktop()`.

- **Placement.** A desktop layer is in the backdrop set (`place_within_backdrop()` is true for it
  too), so it's drawn once, at output coordinates, under the workspaces. `layers_in_render_order`
  yields desktop layers first, which puts them above the rest of the backdrop (the wallpaper)
  whatever order they mapped in.
- **Fade.** `Niri::layer_fade` gives `1 - overview_progress`, which `render_normal` and
  `render_popups` multiply into the rule's opacity. `background_effect::render_for_tile` takes the
  same alpha into each area's `RenderParams.alpha`, so the frost fades with the surface (xray and
  non-xray). At 0 the surface isn't drawn at all. The backdrop xray buffer is filled through the
  same path, so frosted windows see the faded desktop.
- **Popups.** A desktop layer's popups (its menus) are drawn by `render_desktop_popups`, with the
  other Background/Bottom popups above the windows, at output coordinates; the backdrop pass skips
  them. Their grabs and the layer's on-demand keyboard focus are kept (a backdrop layer's are
  dropped).
- **Input.** `contents_under` and `is_layout_obscured_under` hit-test desktop layers at output
  coordinates (no workspace offset): their popups with the Background popups, the surface after the
  workspace's Background layers, both only while the overview is closed, as for other background
  layers.

### 3. `DesktopObscured` IPC event

```rust
DesktopObscured { output: String, obscured: bool }
```

Sent per output when the value changes, and replicated to new clients. When it is true, and how
the coverage test works, is in [ipc/events.md](../ipc/events.md#desktopobscured).

### 4. Client

amane's `Desktop` service holds the event per output. emaki's widgets freeze their surface
(draw nothing, commit nothing) while their output's desktop is obscured, and catch up in one frame
when it shows again. See emaki's `docs/widgets.md`.

### 5. Files

- `niri-config/src/{layer_rule,lib}.rs`, `src/layer/{mod,mapped}.rs`
- `src/niri.rs` (render order, fade, desktop popups, input, focus), `src/handlers/xdg_shell.rs`
- `src/render_helpers/background_effect.rs`, `src/window/mapped.rs` (the effect alpha)
- `src/layout/{mod,monitor,workspace}.rs` (coverage)
- `niri-ipc/src/{lib,state}.rs`, `src/ipc/{server,client}.rs`
