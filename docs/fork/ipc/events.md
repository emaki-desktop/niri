# IPC events

Four events this fork adds to the niri IPC event stream (`niri msg event-stream`, or
`niri msg --json event-stream` for JSON). Three are continuous gesture-progress values, sent
per frame while something moves, so an external bar can animate in step with the compositor.
The fourth tells a [desktop layer](../layout/desktop-layer.md) when it can't be seen.

The original spec, with the reasoning, is [design/gesture-ipc.md](../design/gesture-ipc.md).

## Usage

| Event | Fields | Scope | Sent when |
|---|---|---|---|
| `OverviewProgress` | `progress: f64` | global | the overview open/close progress moves by 0.001 or more |
| `ViewOffsetChanged` | `output: String`, `view_pos: f64`, `active_column_idx: usize` | focused output | `view_pos` moves by 1 logical px or more, or the active column or the focused output changes |
| `WorkspaceSwitchProgress` | `output: String`, `workspace_idx: f64`, `active_workspace_idx: usize` | focused output | `workspace_idx` moves by 0.004 or more (about 250 steps per workspace), or the active workspace or the focused output changes |
| `DesktopObscured` | `output: String`, `obscured: bool` | every output | `obscured` changes on that output |
| `KeyboardLockKeysChanged` | `caps_lock: bool`, `num_lock: bool` | the seat's keyboard | Caps Lock or Num Lock turns on or off |
| `OutputsChanged` | none | global | the answer to `Outputs` was rebuilt: an output connected or went away, or a mode, scale, position, HDR or SDR brightness changed |

As JSON, one event per line:

```json
{"OverviewProgress":{"progress":0.42}}
{"ViewOffsetChanged":{"output":"eDP-1","view_pos":1264.5,"active_column_idx":2}}
{"WorkspaceSwitchProgress":{"output":"eDP-1","workspace_idx":1.37,"active_workspace_idx":1}}
{"DesktopObscured":{"output":"eDP-1","obscured":true}}
{"KeyboardLockKeysChanged":{"caps_lock":true,"num_lock":false}}
{"OutputsChanged":{}}
```

### `OverviewProgress`

The overview's open/close progress: `0.0` closed, `1.0` fully open. It follows the open and
close animation, the four-finger gesture and the hot corner. It is the value the overview
zoom is drawn with, not clamped, so a spring or gesture can take it slightly past either end.
Closed with no animation running, it is `0.0`.

### `ViewOffsetChanged`

The horizontal scroll position of the focused output's active workspace, in logical pixels
(unbounded): `ScrollingSpace::view_pos()`, the active column's x plus the live view offset. It
moves during three-finger horizontal swipes, keyboard column switches, two-finger scrolling in
the overview, touch and tablet drags, and DnD edge auto-scroll.

### `WorkspaceSwitchProgress`

The focused output's workspace position used for rendering, `Monitor::workspace_render_idx()`:
fractional while a switch animates or a three-finger vertical swipe is under way. Swipes
rubber-band at the ends, so it can briefly go past `0` or the last index; clamp it on the
client if that matters.

### `DesktopObscured`

Whether nothing under the workspaces shows on an output, so a desktop layer can stop drawing.
`Layout::is_desktop_obscured(output)` is true when:

- the overview is fully open (progress 1), where desktop layers have faded out; or
- the overview is closed, no workspace switch is in progress on the output
  (`Monitor::is_desktop_covered`), and the active workspace's visible tiles cover its working
  area (`Workspace::is_working_area_covered`).

The coverage test grows each tile by `layout.gaps` on every side, since the mat fills the gaps, and
subtracts the tiles from the working area in whole logical pixels (area rounded inward, tiles
outward). A fullscreen or maximized window covers it, as do columns that fill the view. Windows
are taken as opaque: a translucent or frosted window that covers the area counts as covering.

A switch in progress or an overview anywhere between closed and open counts as visible: either
shows what's under the workspaces.

### `Window.is_fullscreen`

Not an event of its own: a field on `Window`, so `niri msg --json windows`, `focused-window`
and the `WindowOpenedOrChanged` event carry it. It is true while the window is fullscreen (not
windowed fullscreen), and a change sends `WindowOpenedOrChanged`, so a bar can hide for a
fullscreen window. It's `#[serde(default)]`, so clients and servers built without it still read
each other. The server reads it before taking the toplevel's lock (`26290100`).

### `KeyboardLockKeysChanged`

The lock-key LEDs of the seat's keyboard (`KeyboardHandle::led_state()`), so a shell can show a
Caps Lock indicator without polling `/sys/class/leds`. Sent once on connect and then on each
change; Scroll Lock isn't reported.

### `OutputsChanged`

A prompt to ask `Outputs` again. It carries nothing and isn't replicated: a new client asks
`Outputs` once anyway. Sent at the end of `State::refresh_ipc_outputs`, which runs only when the
outputs changed (`ipc_outputs_changed`), so a resting compositor sends none.

### Reading the stream

- **New clients get the current values first.** `EventStreamState::replicate()` sends each
  event's last value on connect, once it has been sent at least once. `DesktopObscured` is
  replicated for every connected output, in name order; an unplugged output is forgotten, and
  gets an event again when it comes back.
- **Order.** Within one refresh the events go out in this order: `WorkspacesChanged` and friends,
  window events, `OverviewOpenedOrClosed`, `OverviewProgress`, `ViewOffsetChanged`,
  `WorkspaceSwitchProgress`, `DesktopObscured`.
- **Drain promptly.** Each client has a buffer of 64 events (`EVENT_STREAM_BUFFER_SIZE`). If it
  fills, niri disconnects the client ("reading events too slowly"). At 60–144 Hz that is under a
  second of backlog, so the reading thread should only store the values and read on, and
  rendering should happen elsewhere.
- **A resting compositor sends nothing.** The epsilons above keep idle frames quiet.
- **Parse by key.** `niri-ipc` isn't semver-stable, so a client can parse the JSON by key
  rather than depend on this fork's crate. Stock niri never sends these events.

amane's `Gestures`, `Overview` and `Desktop` services read these events; see amane's
[compositor services](https://github.com/emaki-desktop/amane/blob/emaki/docs/fork/compositor/services.md).

## Design / internals

| Event | `EventStreamState` part | Refresher (`src/ipc/server.rs`) | Source |
|---|---|---|---|
| `OverviewProgress` | `OverviewProgressState` | `ipc_refresh_overview_progress` | `Layout::overview_progress_value()` |
| `ViewOffsetChanged` | `ViewOffsetState` | `ipc_refresh_view_offset` | `Layout::active_view_state()` |
| `WorkspaceSwitchProgress` | `WorkspaceSwitchState` | `ipc_refresh_workspace_switch` | `Layout::active_view_state()` |
| `DesktopObscured` | `DesktopObscuredState` (per-output map) | `ipc_refresh_desktop_obscured` | `Layout::is_desktop_obscured()` |
| `KeyboardLockKeysChanged` | `LockKeysState` (`niri-ipc/src/fork.rs`) | `ipc_refresh_lock_keys` (`src/ipc/server/fork.rs`), from `State::refresh` | `KeyboardHandle::led_state()` |
| `OutputsChanged` | none | `ipc_outputs_changed` (`src/ipc/server/fork.rs`), from `State::refresh_ipc_outputs` | the rebuilt output map |

- The variants are in `niri_ipc::Event` (`niri-ipc/src/lib.rs`) and the parts in
  `niri-ipc/src/state.rs`. The first three parts keep a `seen` flag, so nothing is replicated
  before the first send.
- `State::ipc_refresh_layout()` calls the refreshers after the stock ones. It runs on every event
  loop pass (`src/niri.rs`), so the events fire at frame cadence while something animates and go
  quiet when nothing moves.
- `Layout::active_view_state()` returns an `ActiveViewState` (`src/layout/mod.rs`) for the
  focused monitor's active workspace, or `None` with no outputs.
- `niri msg event-stream` prints all six (`src/ipc/client.rs`).

Commits: `759c9c3f` (progress events), `12509ec6` (`DesktopObscured`), `bd862d07` (unplugged
outputs forgotten). Tests: `niri-ipc/src/fork.rs` and `src/tests/fork_ipc.rs`.
