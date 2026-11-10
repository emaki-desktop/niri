# Features

Status: implemented in `bebe757d`.

A client asks which of the fork's features this niri has, and the version of each one's contract.
Upstream niri can't parse the request and answers with an error, so an error means stock niri:
the client leaves the fork's protocols, events and actions alone.

## Usage

```sh
niri msg features          # one "name version" per line
niri msg --json features   # [{"name":"blur-shapes","version":2},…]
```

Over the socket, send `"Features"` and read `{"Ok":{"Features":[…]}}`. The list is fixed when
niri is built, so ask once per connection.

## Names

| Name | Version | What it covers |
|---|---|---|
| `blur-shapes` | 2 | the [`niri-blur-shapes-v1`](../effects/blur-shapes.md) global's version |
| `vibrancy` | 1 | the [`niri-vibrancy-v1`](../effects/vibrancy.md) global's version |
| `secure-fill` | 1 | the [`niri-secure-fill-v1`](../input/secure-fill.md) global's version |
| `materials` | 1 | [`materials {}`](../effects/materials.md) and `background-effect { material }` |
| `mat` | 1 | the [workspace mat](../layout/workspace-mat.md) |
| `desktop-layer` | 1 | [`layer-rule { desktop true }`](../layout/desktop-layer.md) |
| `hdr` | 1 | [HDR10 output](../display/hdr.md), its IPC output fields and actions, and `wp-color-management-v1` |
| `move-column-to-index-window-id` | 1 | [`MoveColumnToIndex { window_id }`](actions.md) |
| `window-fullscreen` | 1 | [`Window.is_fullscreen`](events.md) |
| `overview-progress` | 1 | the [`OverviewProgress`](events.md) event |
| `view-offset` | 1 | the [`ViewOffsetChanged`](events.md) event |
| `workspace-switch-progress` | 1 | the [`WorkspaceSwitchProgress`](events.md) event |
| `desktop-obscured` | 1 | the [`DesktopObscured`](events.md) event |
| `lock-keys` | 1 | the [`KeyboardLockKeysChanged`](events.md) event |
| `outputs-changed` | 1 | the [`OutputsChanged`](events.md) event |
| `wallpaper-dim` | 1 | the [`SetWallpaperDim`](actions.md#setwallpaperdim) action |

The protocol versions come from the globals' own constants, so they can't drift. Any other
version goes up by hand in `src/ipc/features.rs` when the feature changes in a way a client can
tell; a new feature adds a name. Files: `niri-ipc/src/fork.rs` (the `Feature` type),
`src/ipc/features.rs` (the list and `niri msg features`).
