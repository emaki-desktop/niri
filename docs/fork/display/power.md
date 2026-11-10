# Power on an OLED laptop

What the fork does, and what it leaves to upstream, to keep an idle or lightly used desktop
cheap on a laptop's OLED panel. emaki drives it: the Black appearance, and the refresh rate the
shell sets for the battery (emaki's `docs/user/features/displays.md`).

## What draws less

- **The wallpaper under Black.** [`SetWallpaperDim`](../ipc/actions.md#setwallpaperdim) dims the
  background layers over a black backdrop, or at 100 doesn't draw them. With emaki's Black
  surfaces the screen behind and between windows is then black, and an OLED leaves black pixels
  off. A hidden wallpaper also leaves the xray buffers empty, so blur samples nothing.
- **No glass under Black.** emaki's shell sends no blur shapes while it shows black surfaces (its
  glass is Solid), so niri runs no blur passes for its surfaces.

## What upstream already does, checked

- **Nothing changes, nothing is drawn.** An output redraws only when something queues a redraw
  (a commit, input, an animation, a config change). A frame with no damage is not submitted; it
  only arms an estimated-vblank timer for clients waiting on frame callbacks, and with none
  waiting the output goes idle (`RedrawState::Idle`). So a still desktop sends the panel no
  frames, and the panel can self-refresh (PSR, PSR2/selective update, Panel Replay).
- **Damage reaches KMS.** smithay's `DrmCompositor` sets `FB_DAMAGE_CLIPS` on the primary plane
  from the frame's damage, which PSR2 and Panel Replay use to send only what changed.
- **Variable refresh idles low.** With VRR on there's no frame to flip while nothing changes, so
  the panel stretches its vblank to its lowest rate (30 Hz on the 2880×1800 120 Hz OLED this was
  written on), as the kernel's `VRR_ENABLED` documents.
- **The fork's blur keeps what didn't change.** An `EffectBuffer` blurs again only where its
  backdrop changed within the blur's reach of what is sampled ([memory.md](../effects/memory.md)).

## What isn't on by default

- **Overlay planes** stay behind `debug { enable-overlay-planes }`, as upstream has them. KWin
  measured 1.4–1.8 W less for fullscreen video with them, but upstream saw performance problems
  and AMD's planes can't take video's YUV formats yet. Measure before turning them on:

  ```kdl
  debug {
      enable-overlay-planes
  }
  ```

## Measuring

emaki's `scripts/power-report [seconds] [label]` averages the battery's draw (unplugged) and
reports the outputs' rate, niri's wakeups per second and, as root, amdgpu's PSR and Panel Replay
state from `/sys/kernel/debug/dri/*/eDP-*/`. Compare at the same brightness:

| Run | What to change |
|---|---|
| Dark vs Black | Emaki Settings → Appearance |
| Wallpaper shown, dimmed, hidden | Appearance → Wallpaper (under Black) |
| 120 Hz, 60 Hz, variable up to 60 | Displays → On battery |
| overlay planes off, on | `debug { enable-overlay-planes }`, with a fullscreen video |

`niri msg action set-wallpaper-dim N` changes the wallpaper by hand for a quick comparison.
