# Memory: what the effects hold on the GPU

What the fork's blur, materials and blur shapes keep in GPU memory, when each part is built and
freed, and the rules that keep it small. The figures are for one 2880×1800 output; a full-output
RGBA texture there is 20.7 MB, and a blur chain (the full-size output plus its halving levels)
about 1.33× that, 26–28 MB.

## Xray buffers

Each output has an [`Xray`](../../../src/render_helpers/xray.rs) with a `background` and a
`backdrop` [`EffectBuffer`](../../../src/render_helpers/effect_buffer.rs) **per render target**
(`Output`, `Screencast`, `ScreenCapture`), so the screencast and screen-capture renders don't
redraw the output's. Each buffer holds, once prepared:

| Part | Size | Built when |
|---|---|---|
| offscreen texture | 20.7 MB | the first frame an area samples the buffer |
| the base blur chain, `blur {}`'s passes/offset | ~26 MB | the first frame an area draws with `blur {}`'s own options |
| one variant chain per distinct `blur-passes`/`blur-offset` pair a material asks for (up to 4, least recently asked for goes first) | ~26 MB each | the first frame a material with that pair samples the buffer |

So every distinct pair costs a chain in every buffer that is sampled. A config whose materials
all use one pair, or whose `blur {}` matches the most common pair, builds the fewest. Noise and
saturation are per-element shader uniforms, not part of a chain.

What the fork changed (commit `7874959a`):

- **The base chain is built only when used.** Upstream's `prepare` always built it. `Xray::render`
  now tells `prepare_with` whether any area draws with the buffer's own options, and an area
  that turns out to fall back to it (a material whose variant is missing) prepares it late in
  `render_with`. With every blurred rule naming a material, nothing uses the base chain and it
  is never built.
- **An empty background over see-through workspaces isn't rendered.** The background buffer
  holds the non-desktop `Background` layers; with the wallpaper and widgets on the desktop
  layer it is empty, and when every workspace's background color is transparent too, drawing
  it would show nothing. `Xray::render` skips preparing it then. That saves the texture and
  its chains in every target.
- **A tick per output frame lets go of what isn't used.** `EffectBuffer::tick` counts rendered
  frames (not time: a still screen renders none, and nothing is lost keeping what the next
  frame wants) and releases variants unused for 600 frames, then the base chain, then the whole
  buffer. `Niri::update_xray_render_elements` ticks every buffer before a frame.
- **The capture targets are released after a one-off render.** `capture_screenshots` and
  `do_screen_transition` render all three targets; before, the screencast and screen-capture
  buffers then stayed for the session (about 300 MB after one screenshot). `Niri::release_capture_xray`
  drops them right after, and the unmap snapshot drops the screencast target it filled. A
  running cast renders its target again next frame and gets them back then.

### A change blurs again only where it's sampled

An `EffectBuffer` kept each blur until its offscreen took damage, then made every chain again
for the whole buffer. The backdrop buffer holds `desktop true` layers (the shell's widgets), so
a widget repainting, a clock's tick, re-blurred the full-screen frost chain behind the bar, a
strip at the top of the screen.

Each blur now keeps a `Stale` list of where the offscreen changed since it was made (folded
into one bounding box past 16 rectangles). `render_with` takes the area its caller samples
(`XrayElement` passes its `src`) and keeps the blur when no change lies within the blur's reach
of that area, about `offset · 2^(passes + 1)` pixels; a caller that doesn't say where it
samples gets a fresh blur, as before. Memory is the same; what it saves is the full-screen
blur passes, on every widget repaint. `NIRI_REBLUR_ALL=1` blurs again on every change.

## Blur-shape captures

A surface's blur shapes draw from one capture of the area behind them all
([`FramebufferEffect`](../../../src/render_helpers/framebuffer_effect.rs)). The capture-only
element blits the framebuffer into a texture the size of the shapes' bounds, blurs it through
its own chain, and shares the blurred texture with the shape elements through `shared` (and one
`SharedCapture` per further blur group).

Two things were wrong (fixed in `7874959a`):

- **The chain was rebuilt every frame.** The shared capture was a clone of the blur's own
  output texture. `Blur::prepare_textures` requires that texture to be its alone and otherwise
  recreates the whole chain, so every capture of a blur-shape surface reallocated the chain.
  The capture element now drops the shared capture before the blur prepares its textures; the
  shapes draw after the capture, so they still see a fresh one.
- **A surface without shapes kept its last capture.** `FramebufferEffect::forget_captures`
  clears `shared` and the groups when the surface renders without shapes (or xray, or with the
  effect off). A surface that unmaps (an amane window hiding) drops the effect with its
  `MappedLayer`.

Check with `RUST_LOG=niri::render_helpers::blur=debug`: `recreating textures: not unique` must
not appear while a blur-shape surface is on screen.

## Window swipe

A tile swiped with Mod and three fingers fades through an [`OffscreenBuffer`](../../../src/render_helpers/offscreen.rs)
(`swipe_offscreen`, a window-sized texture). It stayed for the tile's life; now the tile
clears it on the first frame it renders neither swiped nor fading (`OffscreenBuffer::clear`).

## Measuring

niri's GPU memory is in `/proc/$(pidof niri)/fdinfo/*`, which needs root under
`ptrace_scope 1`:

```sh
sudo awk '/drm-client-id/{id=$2} /drm-total-(vram|gtt):/{m[id SUBSEP $1]=$2}
  END{for(k in m){split(k,a,SUBSEP); t[a[2]]+=m[k]} for(x in t) printf "%s %.0f MiB\n", x, t[x]/1024}' \
  /proc/$(pidof niri)/fdinfo/*
```

Count each DRM client once (the `id` above): a process holds more than one fd on the client,
and summing per fd doubles the figure. Take it at idle, after a screenshot, and after a window
swipe. The DRM scanout swapchain (about three full-output buffers) is the floor.

Measured live on 2026-10-27 with all of the above in: 353 MB of VRAM and 12 MB of GTT at
idle on one 2880×1800 output (there was no per-process reading before the changes; the
research estimated 150 MB of xray buffers steady and 450 MB after a screenshot). After a
screenshot and after a window swipe are still to be read.

## Config

The global `blur {}` should carry the pair most materials use, so that pair draws from the base
chain and no variant is built for it; a new material should reuse an existing pair unless it
needs its own. `hotkey-overlay { skip-at-startup }` saves the overlay's cached texture per
output at every start.

```kdl
blur {
    passes 3
    offset 2
    noise 0.02
    saturation 1.3
}

hotkey-overlay {
    skip-at-startup
}
```
