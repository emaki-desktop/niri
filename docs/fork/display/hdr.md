# HDR

Status: implemented in `ae549528` (HDR10 output) and `ddd3381d` (color management). Shell
Settings' Displays page drives it; see amane-shell's
[displays.md](https://github.com/098765434/amane-shell/blob/main/docs/shell/displays.md).

## Usage

An output whose EDID says it decodes PQ (HDR10) can be driven in HDR:

```kdl
output "eDP-1" {
    hdr
    // how bright SDR white is in HDR mode, in nits (203 by default)
    sdr-brightness 300
}
```

Or only while something on the output is HDR, to save what HDR mode costs the rest of the
time (a 41 MB half-float composition, an encode pass on every change, and no cursor or
fullscreen planes):

```kdl
output "eDP-1" {
    hdr-auto
    sdr-brightness 300
}
```

`hdr` wins when both are set. See [Automatic HDR](#automatic-hdr-hdr-auto) for how it decides.

Or at once, until the config is read again:

```sh
niri msg output eDP-1 hdr on
niri msg output eDP-1 sdr-brightness 300
```

`niri msg outputs` says whether each output supports HDR, whether it's on, its peak and the SDR
brightness; the JSON gains `hdr_supported`, `hdr_enabled`, `sdr_brightness` and
`peak_luminance`.

The desktop looks the same in HDR mode, with SDR white at `sdr-brightness`. Clients that
describe their color through `wp-color-management-v1` show more: HDR video and games keep their
highlights up to the panel's peak. Some clients that do this:

- mpv with `--vo=gpu-next` (`--target-colorspace-hint` follows the output);
- Firefox with `gfx.wayland.hdr` on;
- Vulkan apps through Mesa's Wayland WSI, which asks for HDR10 or scRGB swapchains (Proton
  games with `DXVK_HDR=1` under Wine's Wayland driver, or gamescope).

Turning HDR on or off sets the output up again from scratch (as `off` then `on` does), so the
screen blanks for a moment. Night light (a `wlr-gamma-control` client such as wlsunset) keeps
working in HDR mode.

**Requirements:** a PQ-capable EDID, an output rendered on the primary GPU, GLES 3 (for
half-float textures) and a 10-bit scanout format (`XRGB2101010` or `XBGR2101010`). If one is
missing, niri logs why and keeps the output in SDR. It never leaves a screen black.

## Design / internals

### The composition

Every element and effect shader in niri (and the fork's glass) works on gamma-2.2-encoded values
from 0 to 1, blending them as they are. HDR mode keeps that encoding, so nothing else changes:
the output's elements are rendered, with damage tracking, into a half-float texture
(`Abgr16161616f`, about 41 MB at 2880×1800) where 1.0 is SDR white, values past it are brighter,
and negatives are colors outside BT.709 (`render_helpers/hdr.rs`, `HdrBuffer`).

One pass then encodes it onto the 10-bit scanout buffer (`HdrEncodeElement`,
`shaders/hdr_encode.frag`):

1. Apply the gamma-control ramp, fitted per channel as `gain · x^exponent`. Under PQ the CRTC's
   `GAMMA_LUT` would bend the signal, so it stays linear.
2. Decode the extended gamma 2.2 to linear light.
3. Multiply by `sdr-brightness`.
4. Convert BT.709 to BT.2020.
5. Encode PQ.

The encode element's damage is the composition's, so an idle output still draws nothing. While
HDR is on, nothing goes straight to a plane: a client's or the cursor's sRGB buffer on a PQ plane
would show at the wrong brightness. So the cursor and fullscreen clients are composited too.

`Tty::render` (`backend/tty.rs`) hands the encode element to the DRM compositor in place of the
elements. Frame callbacks, presentation feedback and dmabuf feedback use the states from the
composition's render.

### KMS

`set_hdr` sets three connector properties, with the legacy property ioctl (amdgpu turns these
into a modeset of their own):

- `max bpc` to 10;
- `Colorspace` to `BT2020_RGB`;
- `HDR_OUTPUT_METADATA`: a `hdr_output_metadata` blob with static metadata type 1, the ST 2084
  EOTF, and the EDID's primaries and luminance (BT.2020 when the EDID gives no primaries).

`reset_hdr` puts the connector back to SDR. It runs when the output connects in SDR, when it
disconnects, and before niri leaves the VT, so a text console or greeter is never shown as PQ.
On resume an HDR output gets its HDR back. If the 10-bit compositor can't be made, niri resets
the properties and makes an 8-bit one. If smithay's atomic commits ever fight these properties,
the fix is a small smithay patch that carries them in the atomic state.

### Color management: `wp-color-management-v1`

`src/protocols/color_management.rs`, version 2:

- **Features:** parametric, set_primaries, set_tf_power, set_luminances,
  set_mastering_display_primaries, extended_target_volume and windows_scrgb. No ICC.
- **Transfer functions:** BT.1886, gamma 2.2 and 2.8, sRGB, compound power 2.4, extended
  linear, PQ, and any power from `set_tf_power`.
- **Primaries:** sRGB, PAL-M, PAL, NTSC, generic film, BT.2020, DCI-P3, Display P3, Adobe RGB,
  or custom.
- **Intent:** perceptual only.

An output in HDR mode describes itself as BT.2020 and PQ, its reference white at
`sdr-brightness` and its target luminance the panel's. Any other output is gamma 2.2 sRGB.
Surfaces' preferred description is the HDR output's when there is one. Clients get
`image_description_changed` and `preferred_changed2` when HDR is turned on or off, or when its
brightness changes.

### Converting surfaces

A surface's description is double-buffered with its commits (`ColorCachedState`). One that is
plain sRGB-like (gamma 2.2 or sRGB, sRGB primaries, reference white at the top of its range) is
drawn as before, at no cost. Any other is converted while it's drawn, in `cm_surface`, or in
`clipped_surface_cm` when it's clipped (rounded corners), so the two programs never stack
(`shaders/color_convert.frag`):

1. Decode the transfer function.
2. Scale so the content's reference white is 1.0: PQ in absolute nits over the reference (203
   by default), scRGB with 1.0 at 80 nits and 203 as reference.
3. Convert the primaries to BT.709 with a Bradford-adapted matrix.
4. Roll highlights off smoothly past three quarters of the target's peak, but only when the
   content goes past the target.
5. Encode as the composition's extended gamma 2.2.

Where a render is going rides on the renderer's EGL context (`render_helpers/color.rs`,
`ColorTarget`):

- An HDR output's own render keeps highlights up to the panel's peak over SDR white.
- Every other render, such as screenshots, screencasts and the color picker, has a peak of
  1.0. HDR content in a capture is therefore tone-mapped to SDR, never clipped as raw PQ.

Converted surfaces aren't scanned out. Windows drawn offscreen (open, close and resize
animations, opacity) get half-float buffers while their output is in HDR, so their highlights
survive.

### Automatic HDR: `hdr-auto`

An `hdr-auto` output stays in SDR until a surface it draws is brighter than SDR white: one whose
description's content peak is over its reference white (PQ, scRGB, or a power curve whose
`max` luminance is over its reference). `render_helpers/surface.rs` notes such a surface while
the output's elements are gathered (`hdr::note_bright`), and `tty/hdr.rs` (`follow_content`)
reads the note after each render:

```mermaid
stateDiagram-v2
    SDR --> SDR: no bright surface
    SDR --> HDR: a bright surface drawn for 1 s
    HDR --> HDR: a bright surface drawn
    HDR --> SDR: none drawn for 10 s
```

The delays keep a thumbnail scrolled past, a pause or a seek from blanking the screen twice:
switching is the same reconnect as `hdr` changing in the config (`hdr::wanted` is what
`decide` and `needs_reconnect` ask). A paused video or an idle screen draws nothing, so a
calloop timer looks again when a delay runs out. The state is kept by connector name, so it
outlives the reconnect.

In SDR mode surfaces are told the output is sRGB, so a client that tone-maps to the output's
preferred description, and only sends HDR once the output says it shows HDR, never switches an
`hdr-auto` output on. Which players and games describe their content as it is, and so do, is
still to be checked on this machine.

### Limits

- The glass (blur and xray buffers) stays 8-bit, so HDR highlights behind glass are clipped at
  SDR white inside the glass.
- Only outputs rendered on the primary GPU can do HDR.
- There is no HLG and no ICC.
- The content's light levels are read from its description, not from per-frame metadata.
- Reference white is the only knob: there is no per-application brightness. On panels that ignore
  their backlight under PQ (the amdgpu OLED this was written on), it is also the screen's
  brightness: amane-shell moves `sdr-brightness` from the brightness keys and sliders while HDR is
  on.

## Testing

HDR needs a real TTY session, since nested niri has no KMS. Once it's on:

- `modetest -M amdgpu -c` shows `Colorspace` at `BT2020_RGB`, `max bpc` at 10 and a
  `HDR_OUTPUT_METADATA` blob.
- `wayland-info` lists `wp_color_manager_v1` with its features.
- `mpv --vo=gpu-next --target-colorspace-hint` on an HDR10 sample shows highlights above the
  desktop's white, and a screenshot of it looks like SDR.
