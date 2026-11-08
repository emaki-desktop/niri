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
