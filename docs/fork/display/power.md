# Power on an OLED laptop

What the fork does, and what it leaves to upstream, to keep an idle or lightly used desktop
cheap on a laptop's OLED panel. emaki drives it: the Black appearance, and the refresh rate the
shell sets for the battery (emaki's `docs/user/features/displays.md`).

## What draws less

- **The wallpaper under Black.** [`SetWallpaperDim`](../ipc/actions.md#setwallpaperdim) dims the
  background layers over a black backdrop, or at 100 doesn't draw them. With emaki's Black
  surfaces the screen behind and between windows is then black, and an OLED leaves black pixels
  off. A hidden wallpaper also leaves the xray backdrop empty: glass over it still draws its
  tint, but the backdrop gets no blur chain and runs no blur passes.
- **No glass under Black.** emaki's shell sends no blur shapes while it shows black surfaces (its
  glass is Solid), so niri runs no blur passes for its surfaces.

## Damage, redraws and wakeups the fork cuts

- **Glass reports only what changed** (`2167afb1`). An `EffectBuffer` keeps a `DamageBag` of
  where its contents re-rendered, and `XrayElement::damage_since` returns only what lies within
  the blur's reach, plus the glass's refraction and dispersion, of a change, mapped from its
  `src` into its geometry. Before, any backdrop change damaged every glass element over its whole
  area: a desktop clock ticking under Zen recomposited all of the glass, and Panel Replay's
  selective update grew with it. An opaque region for an opaque `bg_color` is still left out.
- **Re-blurs are scissored** (`0585d391`). The blur chain remembers the source and options it
  last blurred whole, and a re-blur of the same redoes every level only within the changed area
  widened by the reach; elsewhere each level still holds what's right. A first blur, new
  textures, other options and `NIRI_REBLUR_ALL=1` still blur everything. The final
  full-resolution up pass stays, for the look.
- **A re-blur after a nearby change really happens** (`66ce460d`). The kept blur held a clone of
  the chain's output texture, so the next re-blur failed its unique-reference check and the
  stale blur stayed on screen.
- **Commits that can't be seen** (`e75026c9`). A window on a hidden workspace that no screencast
  renders no longer queues its output's redraw. A layer-shell commit re-arranges the layout's
  working area only when it moved or resized a layer or changed the exclusive zone, and a
  content-only commit redraws only if the layer is shown somewhere.
- **Suspended windows** (`e4c97e8e`). Windows on a hidden workspace, and all of them with the
  monitors off, get xdg-shell's `suspended` state, so browsers and toolkits stop animating;
  never a screencast's target, nor while the window switcher shows thumbnails. Windows scrolled
  out of the strip or fully covered aren't suspended yet.
- **An animated cursor redraws only where it shows** (`d10afa30`): not while hidden by typing or
  inactivity, and not on the other outputs.
- **Monitors off and the lid** (`7d641a9a`). With the monitors off, a redraw skips
  `update_render_elements` unless a screencast streams. Closing the lid with no external display
  turns the monitors off (as on idle) when nothing suspended the machine, and opening it turns
  them back on.
- **Timers** (`0bf3eaa1`). The XDG activation token sweep is armed when a token is made and
  stops once none remain; the 1 s fallback frame-callback timer stops itself while the monitors
  are off.
- **Per-iteration work** (`44e5c208`). The fork's IPC refreshes borrow output names instead of
  copying them on every loop iteration, and the vblank message for Tracy is only formatted when
  built with Tracy.
- **`hdr-auto`** (`2657cf7b`, `05cc0b26`). It decides from the output's own frame, not a
  screencast's or screenshot's, and `niri msg output … hdr off` pauses it until the config is
  read again, which is how Emaki keeps the panel in SDR on battery.
- **BT.1886 SDR video draws as is** (`4945c7dd`), with sRGB primaries, as desktops without
  color management show it, so it keeps direct scanout.

Not done yet: a layer under windows counts as visible through xray wherever it is (F3 in the
emaki research), the loop's upstream `refresh()` still runs on every iteration, scrolled-away
tiles still build elements, and `fifo-v1`, `commit-timing-v1` and `content-type-v1` aren't
wired (they need frame-scheduling changes, not just the globals).

## Config watcher

Upstream checks the config and its includes for changes every 500 ms, which woke a thread twice a
second all session: on a still desktop about half of niri's wakeups. The fork
(`src/utils/watcher/fork.rs`) watches their directories with inotify instead, and an event there
wakes the same check (a changed mtime or symlink target), 100 ms after it so a burst of writes is
one check. What it watches:

- the directory of the config, of each include, and of where each resolves to, or the nearest
  directory above that exists, so one made later is seen being made;
- not the Nix store: nothing there changes in place, and a new generation is a symlink swapped in
  a watched directory (`~/.config/niri/config.kdl`).

The check still runs every 10 s for what the watches can't see, such as a symlinked directory
swapped higher up, and every 500 ms, as upstream, when inotify isn't available.

## What upstream already does, checked

- **Nothing changes, nothing is drawn.** An output redraws only when something queues a redraw
  (a commit, input, an animation, a config change). A frame with no damage is not submitted; it
  only arms an estimated-vblank timer for clients waiting on frame callbacks, and with none
  waiting the output goes idle (`RedrawState::Idle`). So a still desktop sends the panel no
  frames, and the panel can self-refresh (PSR, PSR2/selective update, Panel Replay).
- **Damage reaches KMS.** smithay's `DrmCompositor` sets `FB_DAMAGE_CLIPS` on the primary plane
  from the frame's damage, which PSR2 and Panel Replay use to send only what changed.
- **Variable refresh idles low, at a price.** With VRR on there's no frame to flip while nothing
  changes, so the panel stretches its vblank to its lowest rate (30 Hz on the 2880×1800 120 Hz
  OLED this was written on), as the kernel's `VRR_ENABLED` documents. But amdgpu turns PSR and
  Panel Replay off while VRR is enabled, and that costs more than the lower rate saves: measured
  on that laptop (kernel 7.2, a still screen), 120 Hz with Replay drew 3.81 W, 120 Hz with VRR
  4.00 W and no Replay, and 60 Hz 3.87 W, also without Replay. So leave VRR to games and video
  (`on-demand=true`, which with no window rule asking for it never turns VRR on).
- **The fork's blur keeps what didn't change.** An `EffectBuffer` blurs again only where its
  backdrop changed within the blur's reach of what is sampled ([memory.md](../effects/memory.md)),
  and since `2167afb1` the glass on screen is damaged only there too (above).

## What isn't on by default

- **Overlay planes** stay behind `debug { enable-overlay-planes }`, as upstream has them. KWin
  measured 1.4–1.8 W less for fullscreen video with them, but upstream saw performance problems
  and AMD's planes can't take video's YUV formats yet (video needs the underlay arrangement,
  the video on the primary plane). Measure before turning them on, with fullscreen 1080p video in
  a browser, unplugged, comparing the battery's draw and checking for dropped frames:

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
