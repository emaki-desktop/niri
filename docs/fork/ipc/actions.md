# `MoveColumnToIndex` by window

Status: implemented in `3a984abc`.

An optional `window_id` on the `MoveColumnToIndex` IPC action. With it, niri moves the column
holding that window to the index, on whichever workspace the window is on, without changing the
active column or the focus. Without it, the action does what it always has and moves the focused
column.

emaki's dock needs this. Dragging a window's icon to a new place among its workspace's
icons reorders that window's column. If the drag focused the window first, every reorder would
switch the view to that workspace and column.

## Usage

From the command line, or a script:

```sh
niri msg action move-column-to-index --window-id 42 3
```

As an IPC request:

```json
{"Action":{"MoveColumnToIndex":{"window_id":42,"index":3}}}
```

`index` counts from 1 and is clamped to the workspace's columns. A floating window has no column,
so nothing happens. Neither does an unknown id, or a window that's being moved interactively.
Without `--window-id` the action is stock: it moves the focused column. Window ids come from
`niri msg windows` or the `WindowsChanged` / `WindowOpenedOrChanged` events.

Moving a window to another workspace and then to a place there takes two requests:
`MoveWindowToWorkspace { window_id, reference, focus: false }`, then this. amane's
`Windows::move_to` sends both, in order, on its worker thread.

There is no config-bind form: `MoveColumnToIndexById` is IPC only.

## Design / internals

### Implementation

| Layer | What |
|---|---|
| `niri-ipc/src/lib.rs` | `window_id: Option<u64>` on `Action::MoveColumnToIndex`, as on `MoveWindowToWorkspace` |
| `niri-config/src/binds.rs` | `Action::MoveColumnToIndexById { window_id, index }` (`#[knuffel(skip)]`, IPC only), which the IPC action turns into when it has an id |
| `src/input/mod.rs` | finds the window by its IPC id and calls the layout |
| `src/layout/mod.rs` | `Layout::move_column_with_window_to_index` finds the workspace that holds the window |
| `src/layout/workspace.rs` | passes it to the scrolling space |
| `src/layout/scrolling.rs` | `ScrollingSpace::move_column_with_window_to_index` does the move |

When the window's column is the active one, the scrolling space uses the existing
`move_column_to`. Otherwise it removes the column and its `ColumnData` and puts them back at the
new index. It then moves `active_column_idx` by one if the move went past the active column, so
the same column stays active, and offsets `view_offset` by how far the active column moved, so
the camera stays where it was. Both columns animate from where they were, the same way
`move_column_to` animates them (`animate_move_from`).

### Tests

- `move_column_with_window_to_index_keeps_focus` in `src/layout/tests.rs`: three columns, focus
  on the middle one, the first column moved to the end and back. The order changes, the focus
  doesn't.
- `Op::MoveColumnWithWindowToIndex` joins the randomized layout operations.

### Compatibility

Upstream niri doesn't reject unknown fields, so it accepts the request, ignores `window_id`, and
moves the **focused** column. A client must only send the id to this fork.

Upstreamable: likely. It's small, optional, and matches how `MoveWindowToWorkspace` already
takes a `window_id`.

# `SetWallpaperDim`

Status: implemented for emaki's Black appearance (OLED energy saving).

Dims the wallpaper by a percentage over a black backdrop. At 100 the wallpaper isn't drawn at
all. On an OLED panel the pixels behind the windows then stay off, and blur (xray and blur
shapes) has no wallpaper to sample. The setting lasts until it's set again or niri restarts; it
isn't in the config, because the shell owns it (it follows `appearance.json`'s Black).

## Usage

```sh
niri msg action set-wallpaper-dim 100   # hide the wallpaper
niri msg action set-wallpaper-dim 75    # dim it
niri msg action set-wallpaper-dim 0     # as configured
```

As an IPC request:

```json
{"Action":{"SetWallpaperDim":{"percent":75}}}
```

The feature name is `wallpaper-dim` ([features.md](features.md)). It's allowed while the screen
is locked, so the lock screen's backdrop changes along with the desktop's.

## Design / internals

| Layer | What |
|---|---|
| `niri-ipc/src/lib.rs` | `Action::SetWallpaperDim { percent: u8 }` |
| `niri-config/src/binds.rs` | `Action::SetWallpaperDim(u8)` (`#[knuffel(skip)]`, IPC only) |
| `src/input/mod.rs` | calls `Niri::set_wallpaper_dim` |
| `src/niri/fork.rs` | `ForkStates::wallpaper_dim`, `set_wallpaper_dim`, `black_backdrop`, and the wallpaper's share of `layer_fade` |
| `src/niri.rs` | one `black_backdrop` call where each output's backdrop colour is worked out (on config reload and when an output is added) |

"The wallpaper" is every background-layer surface that isn't a [desktop layer](../layout/desktop-layer.md):
`layer_fade` gives it `1 - percent / 100`, so it draws over the backdrop at that opacity, and at 0
`render_layer_normal` skips it, both in the output's render and in the xray buffers it fills. While
`percent` is above 0 each output's backdrop (`overview.backdrop-color`, or the output's own) is
black, so the dimmed wallpaper fades to black rather than to the backdrop colour. Desktop layers
(the shell's widgets) aren't dimmed. The workspace mat and `background-color` are the config's.
