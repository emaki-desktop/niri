# Blur shapes (`niri-blur-shapes-v1`)

Status: implemented in `113728ce` (shapes), `e843c4be` (materials), `39904a0b` (one element id
per shape), `379f1071` (backdrop readings, protocol version 2), `c0c39971` (per-material blurs)
and `5f986fdf` (capture reach). The materials themselves are in [materials.md](materials.md).

## Usage

This fork offers `niri-blur-shapes-v1`: instead of a region of rectangles, a surface sends rounded rectangles, each with its own corner radii and opacity.
niri draws each with antialiased corners at its opacity, so a client with many rounded pieces of glass (such as a quick settings panel) gets blur exactly in their shape that fades with them.
A surface with blur shapes is treated like one with a blur region: blur is on unless a rule says `blur false`.

A shape can also name a [material](materials.md), a glass look niri draws over the blurred background.

## Design / internals

What the shapes' captures hold on the GPU, and when they are dropped, is in
[memory.md](memory.md).

### Why

`ext-background-effect-v1` describes blur as a region of integer rectangles: no
rounded corners and no fade. A shell with many rounded pieces of glass (a
quick settings panel, cards that swell open) gets square, all-or-nothing blur. This
fork adds a private protocol in which the client describes its blur as rounded
rectangles, each with its own opacity and look.

### Protocol — `niri-blur-shapes-v1`

`resources/niri-blur-shapes-v1.xml`, handled in `src/protocols/blur_shapes.rs`.

- `niri_blur_shapes_manager_v1`
  - `get_blur_shapes(id, surface)` creates the per-surface object.
  - Event `material(id, name)` is sent for each configured material, then `done`.
    These events are sent again on config reload.
- `niri_blur_shapes_v1`
  - `reset` clears the pending shapes.
  - `add_rounded_rect(x, y, width, height, radius_top_left, radius_top_right, radius_bottom_right, radius_bottom_left, alpha, material, tint)`
    takes `fixed` values in surface-local logical px. The material is an id from
    the manager (0 = plain blur), and the tint is packed RGBA.
  - The state is double-buffered and applies on `wl_surface.commit`.
  - Event `backdrop(index, luminance)` (version 2) reports how bright the
    blurred backdrop behind a shape is, 0 to 1, for materials with
    `report-backdrop`. The surface's resource is kept in its data map
    (`BlurShapesSurfaceData`) so it can be sent from the renderer.

A surface with blur shapes is treated like one with a blur region: blur is on
unless a rule says `blur false`.

### Rendering

- Each shape is drawn through the existing corner-rounding shader, half a
  physical pixel inside the shape, so it stays under the client's own
  antialiased edge, at its alpha.
- **Xray:** one quad per shape from the shared blurred background
  (`src/render_helpers/xray.rs`). A material with a blur of its own
  (`blur-passes`, `blur-offset`) draws from a second blur of the same
  background, which the `EffectBuffer` keeps beside its own (up to four, redone
  only when the background changes).
- **Non-xray:** the area behind all of a surface's shapes is captured and
  blurred once, and each shape is drawn from that capture
  (`src/render_helpers/framebuffer_effect.rs`). This is needed because smithay
  re-captures duplicated framebuffer effects every frame. Shapes whose
  materials blur differently are grouped, one capture per blur, of the area
  behind that group's shapes. Every capture element comes after every shape in
  the element list, so all are taken before any shape draws and none sees
  another's glass.
- Each shape drawn from one capture gets its own element id. A shared id made
  smithay pair instances by geometry, and a frame could keep a stale rectangle
  of frost (`39904a0b`).

### Materials

Shapes pick a named material by id. The `materials {}` config, the glass shader, backdrop
readings and materials on rules are in [materials.md](materials.md).

### Touched files

`resources/niri-blur-shapes-v1.xml`, `src/protocols/{blur_shapes,raw,mod}.rs`,
`src/render_helpers/backdrop.rs`,
`src/handlers/background_effect.rs`,
`src/render_helpers/{background_effect,xray,framebuffer_effect}.rs`,
`src/render_helpers/shaders/{glass.frag,mod.rs}`, `src/niri.rs`,
`niri-config/src/{appearance,lib,window_rule}.rs`, `src/{layer,window}/mapped.rs`.

### Client

amane's `LayerWindow::blur_shapes` (fork,
[emaki-desktop/amane](https://github.com/emaki-desktop/amane), `docs/fork/blur.md`). It
falls back to an `ext-background-effect` region on other compositors.
