# niri fork docs

A fork of [niri](https://github.com/niri-wm/niri), the scrollable-tiling Wayland compositor, run
under the [amane-shell](https://github.com/098765434/amane-shell) bar. It adds glass
effects, layout extras, a gesture and IPC for the shell; everything else is stock upstream.

Branch `ocbk`, based on the upstream release `v26.04`. Rebasing, branches and the full commit
log: [maintaining.md](maintaining.md).

## Features

| Feature | Summary | Config / IPC surface | Doc | Status |
|---|---|---|---|---|
| Materials | named glass looks drawn over the blur | `materials { material "…" { … } }`; `background-effect { material "…" }` in rules | [effects/materials.md](effects/materials.md) | implemented, `e843c4be`, `379f1071`, `c0c39971`, `bf313b9b`, `5f986fdf` |
| Blur shapes | rounded, fading blur shapes, each with a material and tint | protocol `niri-blur-shapes-v1` | [effects/blur-shapes.md](effects/blur-shapes.md) | implemented, `113728ce`, `39904a0b`, `379f1071` |
| Vibrancy | a subsurface blended with the backdrop (plus-lighter / plus-darker) | protocol `niri-vibrancy-v1` | [effects/vibrancy.md](effects/vibrancy.md) | implemented, `14ed7262`, `9cdf1c8d` |
| Effects memory | what xray buffers, blur chains and shape captures hold, built and freed when used | — | [effects/memory.md](effects/memory.md) | implemented, `7874959a` |
| Workspace mat | a backing behind the scrolling layout that fills the gaps | `layout { mat { … } }` | [layout/workspace-mat.md](layout/workspace-mat.md) | implemented, `7e622e2d`, `3696f762`, `9165052c` |
| Gaps | gaps between workspaces, and between windows in the overview | `layout { workspace-gap }`, `overview { workspace-gap window-gap }` | [layout/gaps.md](layout/gaps.md) | implemented, `9e57324e`, `f4e61d28`, `193f7663` |
| Desktop layer | a background layer that stays still on the wallpaper as the desktop | `layer-rule { desktop true }` | [layout/desktop-layer.md](layout/desktop-layer.md) | implemented, `12509ec6` |
| HDR | HDR10 output with the SDR desktop unchanged, and color-managed HDR surfaces | `output { hdr; sdr-brightness }`, `niri msg output … hdr`, protocol `wp-color-management-v1` | [display/hdr.md](display/hdr.md) | implemented, `ae549528`, `ddd3381d` |
| Mod swipe | Mod-held three-finger swipes close, maximize and resize the focused window | `gestures { mod-swipe { off } }` | [gestures/mod-swipe.md](gestures/mod-swipe.md) | implemented, `823b718a` |
| IPC events | per-frame gesture progress, when the desktop is obscured, whether a window is fullscreen, Caps Lock and Num Lock, and when the outputs changed | `OverviewProgress`, `ViewOffsetChanged`, `WorkspaceSwitchProgress`, `DesktopObscured`, `Window.is_fullscreen`, `KeyboardLockKeysChanged`, `OutputsChanged` | [ipc/events.md](ipc/events.md) | implemented, `759c9c3f`, `12509ec6`, `26290100`, `989d475e` |
| IPC actions | move a window's column without moving the focus | `MoveColumnToIndex { window_id }`, `niri msg action move-column-to-index --window-id` | [ipc/actions.md](ipc/actions.md) | implemented, `3a984abc` |
| Features | which of these the running niri has, with each contract's version; stock niri answers with an error | `Request::Features`, `niri msg features` | [ipc/features.md](ipc/features.md) | implemented, `bebe757d` |
| Secure fill | a password manager fills the focused field only if it's still the one it checked, and learns its owner from the socket | protocol `niri-secure-fill-v1` | [input/secure-fill.md](input/secure-fill.md); `resources/niri-secure-fill-v1.xml` | implemented, `85a7be33` |

Each feature doc starts with **Usage** (config, protocol or IPC), then **Design / internals**.

## Groups

- **Effects:** glass. [Materials](effects/materials.md) are the looks,
  [blur shapes](effects/blur-shapes.md) are how a client asks for them in its own shapes, and
  [vibrancy](effects/vibrancy.md) blends a client's labels with the glass under them.
  [Memory](effects/memory.md) is what all of it holds on the GPU and when it lets go.
- **Layout:** the scrolling layout's surroundings. The [workspace mat](layout/workspace-mat.md)
  fills the gaps, [gaps](layout/gaps.md) sets the space between workspaces and overview windows,
  and the [desktop layer](layout/desktop-layer.md) keeps widgets on the wallpaper.
- **Display:** [HDR](display/hdr.md), the output in HDR10 and clients' color described through
  `wp-color-management-v1`.
- **Gestures:** [Mod swipe](gestures/mod-swipe.md), touchpad swipes on the focused window.
- **Input:** [Secure fill](input/secure-fill.md), checked password fill for the shell's input method.
- **IPC:** [events](ipc/events.md) a bar animates from, [actions](ipc/actions.md) a dock
  sends, and [features](ipc/features.md), how a client tells the fork from stock niri.

The protocol specs are authoritative for the wire format:
[`niri-blur-shapes-v1.xml`](../../resources/niri-blur-shapes-v1.xml),
[`niri-vibrancy-v1.xml`](../../resources/niri-vibrancy-v1.xml) and
[`niri-secure-fill-v1.xml`](../../resources/niri-secure-fill-v1.xml).

## Used by

- **amane** (fork, which amane-shell is built with): the client side of blur shapes, vibrancy, the desktop layer and
  the IPC events and actions. See amane's
  [fork docs](https://github.com/098765434/amane/blob/ocbk/docs/fork/README.md).
- **amane-shell** (the bar, dock and widgets): see its
  [docs](https://github.com/098765434/amane-shell/blob/main/docs/README.md).

The workspace mat, gaps and mod swipe are used only by niri. HDR is set from amane-shell's
Shell Settings, over IPC and the config.

## Design history

Specs kept as written, with a status header:

- [design/gesture-ipc.md](design/gesture-ipc.md): the original spec for the gesture-progress
  IPC events.
