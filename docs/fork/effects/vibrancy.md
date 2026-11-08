# Vibrancy

Status: implemented in `14ed7262`, with plus-darker reworked to subtract in `9cdf1c8d`. The client side
is amane's vibrant layer.

## Usage

There is nothing to configure: `niri-vibrancy-v1` is a protocol a client uses, advertised to
every client. A client that wants vibrant labels on a material:

1. Gives the material surface its frost and tint, for example with
   [blur shapes](blur-shapes.md) and a [material](materials.md).
2. Puts the labels, glyphs and fills in a subsurface above it, in greyscale only.
3. Binds `niri_vibrancy_manager_v1`, calls `get_vibrancy` for the subsurface, then
   `set_mode(plus_lighter)` for dark appearance or `set_mode(plus_darker)` for light, and
   commits the subsurface.

For `plus_darker`, the subsurface holds how much to darken, not the color: a grey `c` at alpha
`a` is drawn as white, `1 − c`, at alpha `a`. Without the global (stock niri) there is no blend, so a client
should draw its labels normally there. amane's vibrant layer does all of this; see amane's
[fork docs](https://github.com/emaki-desktop/amane/blob/emaki/docs/fork/README.md).

## Design / internals

### Why

Labels on a material can be *vibrant*: they're blended with the frosted backdrop under them
(plus-lighter in dark, plus-darker in light), so they take on its color. A client can't do this
itself, because it never sees what's behind its surface. Translucent label colors come close,
since the backdrop shows through them, but they only fade towards it. They don't brighten or
darken it.

### Protocol: `niri-vibrancy-v1`

`resources/niri-vibrancy-v1.xml`, handled in `src/protocols/vibrancy.rs`.

- `niri_vibrancy_manager_v1.get_vibrancy(id, surface)` makes the per-surface object. A second
  one while the first lives is the `already_constructed` error.
- `niri_vibrancy_v1.set_mode(none | plus_lighter | plus_darker)` is double-buffered and applies
  on `wl_surface.commit`. Destroying the object sets `none` on the next commit.

It's meant for a subsurface that holds only the greyscale vibrant content (labels, glyphs,
fills), above a surface whose background is a material.

### Rendering

`push_elements_from_surface_tree` (`src/render_helpers/surface.rs`) wraps every surface's
element in a `VibrantSurfaceRenderElement` (`src/render_helpers/vibrant.rs`), with the mode the
surface's state has now. With `none` it is the plain element.

- **plus_lighter** draws with `glBlendFunc(ONE, ONE)`: the surface is added to what's behind,
  clamped.
- **plus_darker** draws with `glBlendEquationSeparate(FUNC_REVERSE_SUBTRACT, FUNC_ADD)` and
  `glBlendFunc(ONE, ONE)`: the surface is taken away from what's behind, clamped at 0. The
  client draws how far each pixel darkens, `a · (1 − c)` for a grey `c`, so the result is
  plus-darker as it's defined, `D + c − 1`. Like plus-lighter it needs only blend state, so it works
  under any texture program, the rounded-corner one too. The equation is put back to
  `FUNC_ADD` after the draw.
- Alpha is kept as it was (`ZERO, ONE`).
- A vibrant element reports no opaque region, because smithay draws an opaque region with
  blending off and then puts its own blend function back. It also has no underlying storage,
  so it can't be scanned out past the blend.

The elements are drawn back to front, so when a vibrant subsurface is drawn the framebuffer
already holds the frosted backdrop, the material's tint and its parent surface.

### Not done

- Screencasts and screenshots render the same elements, so they show the blend too. Nothing
  special is done for them.

### Files

- `resources/niri-vibrancy-v1.xml`, `src/protocols/{vibrancy,raw,mod}.rs`
- `src/render_helpers/{vibrant,surface,clipped_surface,mod}.rs`
- `src/niri.rs` (the global and the render element types), `src/{layer,window}/mapped.rs`,
  `src/layout/{mod,scrolling,workspace}.rs`, `src/handlers/background_effect.rs`,
  `src/input/mod.rs`

### Commits

- `14ed7262` adds the protocol, with plus-lighter and plus-darker.
- `9cdf1c8d` makes plus-darker subtract (`FUNC_REVERSE_SUBTRACT`), as its formula says.
