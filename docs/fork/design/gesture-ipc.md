# Spec: Live gesture-progress IPC events for a niri fork

Status: implemented in `759c9c3f` · Target: niri fork + Amane shell client · Scope: expose per-frame
gesture progress over the niri IPC event stream so an external bar can animate in sync.

This is the original spec, kept as design history. The reference for the events as built is
[ipc/events.md](../ipc/events.md).

## 1. Goal

Stream three continuous, per-frame values out of niri so a layer-shell bar (Amane) can
track compositor gestures as they happen:

| # | Signal | Gesture | Value emitted | Range / units |
|---|--------|---------|---------------|---------------|
| 1 | Overview progress | 4-finger swipe up/down, hot corner, toggle | `progress: f64` | `0.0` closed … `1.0` fully open |
| 2 | Horizontal view offset | 3-finger horizontal swipe, column focus | `view_pos: f64`, `active_column_idx: usize` | logical px (unbounded) + index |
| 3 | Vertical workspace switch *(bonus)* | 3-finger vertical swipe, workspace focus | `workspace_idx: f64`, `active_workspace_idx: usize` | fractional ws index + index |

All three already exist inside niri to drive rendering. This work exposes them; it does
not compute anything new.

## 2. Architecture & hard constraints

- **The IPC event stream broadcasts to all clients through one method**,
  `IpcServer::send_event(event)` in `src/ipc/server.rs`. Events are produced from
  `State::ipc_refresh_*` methods, aggregated by `State::ipc_refresh_layout()`.
- **Backpressure will drop a slow client.** Each client has a bounded channel of
  `EVENT_STREAM_BUFFER_SIZE = 64`. If it fills (client reads too slowly), `send_event`
  **disconnects that client** ("reading events too slowly"). Per-frame events at 60–144 Hz
  are fine *only* if the client drains promptly (§7).
- **Coalesce before sending.** Emit only when the value changed past an epsilon, so a
  resting compositor emits nothing.
- **Initial state is replicated.** New event-stream clients get the current state up front
  via `EventStreamState::replicate()`. Every new `Event` variant must be handled by a
  matching `EventStreamState` part (`apply` + `replicate`), or the stream state desyncs.
- **`niri-ipc` is not semver-stable**; it tracks niri's version. The client should parse
  raw JSON by key (§7) rather than depending on the forked crate.
- **Signals 2 & 3 are per-output.** Emit for the focused output and include an `output`
  field. Signal 1 (overview) is global.

## 3. Shared plumbing (do once)

### 3.1 Add event variants — `niri-ipc` crate, `Event` enum

```rust
/// Overview open/close animation progress. 0.0 = closed, 1.0 = fully open. Global.
OverviewProgress { progress: f64 },

/// Horizontal scroll position of the focused output's active workspace.
ViewOffsetChanged { output: String, view_pos: f64, active_column_idx: usize },

/// Vertical (fractional) workspace position of the focused output.
WorkspaceSwitchProgress { output: String, workspace_idx: f64, active_workspace_idx: usize },
```

### 3.2 Extend `EventStreamState` — `niri-ipc` crate, `state` module

Add parts mirroring the existing `overview` part. Each stores the last value and
implements the same `EventStreamStatePart` trait the other parts use (`apply` consumes the
matching variant; `replicate` emits the current value). Sketch:

```rust
#[derive(Debug, Default)]
pub struct ViewOffsetState {
    pub output: String,
    pub view_pos: f64,
    pub active_column_idx: usize,
    pub seen: bool,
}

impl EventStreamStatePart for ViewOffsetState {
    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::ViewOffsetChanged { output, view_pos, active_column_idx } => {
                self.output = output;
                self.view_pos = view_pos;
                self.active_column_idx = active_column_idx;
                self.seen = true;
                None
            }
            other => Some(other),
        }
    }
    fn replicate(&self) -> Vec<Event> {
        if !self.seen { return Vec::new(); }
        vec![Event::ViewOffsetChanged {
            output: self.output.clone(),
            view_pos: self.view_pos,
            active_column_idx: self.active_column_idx,
        }]
    }
}
```

Do the same for an `OverviewProgressState { progress }` and a
`WorkspaceSwitchState { output, workspace_idx, active_workspace_idx }`. Register all three
in `EventStreamState` and its `Default`/iteration.

### 3.3 Hook point — `src/ipc/server.rs`, `impl State`

`ipc_refresh_layout` already calls the per-topic refreshers. Add the new ones there:

```rust
pub fn ipc_refresh_layout(&mut self) {
    self.ipc_refresh_workspaces();
    self.ipc_refresh_windows();
    self.ipc_refresh_overview();            // existing: boolean open/closed
    self.ipc_refresh_overview_progress();   // NEW: signal 1
    self.ipc_refresh_view_offset();         // NEW: signal 2
    self.ipc_refresh_workspace_switch();    // NEW: signal 3
}
```

`ipc_refresh_layout` is already invoked in the compositor's refresh/redraw path, which runs
every frame while an animation or gesture is ongoing — so these fire at frame cadence for
free, and go quiet when nothing moves.

## 4. Signal 1 — Overview progress (global)

### 4.1 Source of truth (EXISTS)

- `src/layout/mod.rs` — the layout holds `super::OverviewProgress`, an enum with
  `Animation(Animation)`, `Gesture { value: f64 }`, `Open`.
- `src/layout/monitor.rs` — `OverviewProgress::value() -> f64` and `clamped_value() -> f64`
  already return the number (`1.0` = fully in overview). A `#[cfg(test)]`
  `Monitor::overview_progress_value()` already exists as a reference.

### 4.2 Accessor to ADD — `impl Layout`

```rust
/// Global overview progress: 0.0 closed .. 1.0 fully open.
pub fn overview_progress_value(&self) -> f64 {
    // Read the layout's own OverviewProgress; None => 0.0, else .clamped_value()
}
```

### 4.3 Emit — `impl State`

```rust
pub fn ipc_refresh_overview_progress(&mut self) {
    let Some(server) = &self.niri.ipc_server else { return };
    let progress = self.niri.layout.overview_progress_value();

    let mut state = server.event_stream_state.borrow_mut();
    let part = &mut state.overview_progress;
    if (part.progress - progress).abs() < 0.001 { return; } // coalesce
    part.progress = progress;
    part.seen = true;

    server.send_event(Event::OverviewProgress { progress });
}
```

## 5. Signal 2 — Horizontal view offset (per focused output)

### 5.1 Source of truth (EXISTS)

- `src/layout/scrolling.rs` — `ScrollingSpace::view_pos() -> f64` (public):
  `column_x(active_column_idx) + view_offset.current()`. `view_offset.current()` includes
  the live `ViewOffset::Gesture`, so this moves every frame during a horizontal swipe. Also
  animates on keyboard column switches. Units: logical px.
- Touchpad normalization constant: `VIEW_GESTURE_WORKING_AREA_MOVEMENT = 1200.` (px of
  finger travel per working-area width).
- `Workspace::scrolling() -> &ScrollingSpace` (public — already used in monitor.rs).

### 5.2 Accessor to ADD — `src/layout/scrolling.rs`

```rust
// active_column_idx is a private field; expose it read-only.
pub fn active_column_idx(&self) -> usize { self.active_column_idx }
```

### 5.3 Accessor to ADD — `impl Layout`

Walk to the focused output's monitor (Layout already tracks the active output; it is used
by `Request::FocusedOutput` and by the gesture methods that take `&Output`). Return the
active workspace's scrolling values:

```rust
pub struct ActiveViewState {
    pub output: String,
    pub view_pos: f64,
    pub active_column_idx: usize,
    pub workspace_idx: f64,          // for signal 3
    pub active_workspace_idx: usize, // for signal 3
}

impl Layout {
    pub fn active_view_state(&self) -> Option<ActiveViewState> {
        let mon = /* focused Monitor */;
        let ws  = mon.active_workspace_ref();
        Some(ActiveViewState {
            output: mon.output_name().clone(),
            view_pos: ws.scrolling().view_pos(),
            active_column_idx: ws.scrolling().active_column_idx(),
            workspace_idx: mon.workspace_render_idx(),
            active_workspace_idx: mon.active_workspace_idx(),
        })
    }
}
```

> Note: during a vertical workspace-switch gesture the "active workspace" is ambiguous;
> for the horizontal value, reading the active workspace's scrolling space is correct and
> matches what's rendered.

### 5.4 Emit — `impl State`

```rust
pub fn ipc_refresh_view_offset(&mut self) {
    let Some(server) = &self.niri.ipc_server else { return };
    let Some(v) = self.niri.layout.active_view_state() else { return };

    let mut state = server.event_stream_state.borrow_mut();
    let part = &mut state.view_offset;
    let changed = (part.view_pos - v.view_pos).abs() >= 1.0   // 1px epsilon
        || part.active_column_idx != v.active_column_idx
        || part.output != v.output;
    if !changed { return; }
    part.output = v.output.clone();
    part.view_pos = v.view_pos;
    part.active_column_idx = v.active_column_idx;
    part.seen = true;

    server.send_event(Event::ViewOffsetChanged {
        output: v.output,
        view_pos: v.view_pos,
        active_column_idx: v.active_column_idx,
    });
}
```

### 5.5 Optional: pixel-exact column geometry (low frequency)

**Not built.** `ColumnsGeometryChanged` was never added; the idea is kept here for reference.

`view_pos` alone is enough for parallax / sliding an indicator. To place one bar item per
column *exactly*, the client also needs each column's x and width (same coordinate space;
on-screen left edge of column *i* = `col_x[i] - view_pos`). This changes only when columns
are added/removed/resized — **emit it on layout change, not per frame**. niri already
tracks it (`ScrollingSpace::column_xs`, `ColumnData.width`). Add a
`Event::ColumnsGeometryChanged { output: String, columns: Vec<(f64, f64)> }` (x, width) and
refresh it from the existing `ipc_refresh_windows` change path.

## 6. Signal 3 — Vertical workspace switch (per focused output)

### 6.1 Source of truth (EXISTS)

- `src/layout/monitor.rs` — `Monitor::workspace_render_idx() -> f64` (public): the current,
  possibly fractional, workspace position actually used for rendering (includes the
  zoom/overview correction term). This is the ideal value: during a 3-finger vertical swipe
  it moves continuously between integer workspace indices.
- `WorkspaceSwitch::current_idx() -> f64` and `target_idx() -> f64` (public) if you want the
  raw switch value instead.
- Touchpad constant: `WORKSPACE_GESTURE_MOVEMENT = 300.` (px per workspace). Ends are
  rubber-banded (`WORKSPACE_GESTURE_RUBBER_BAND { stiffness: 0.5, limit: 0.05 }`), so the
  value can slightly overshoot past `0` / `len-1` during a gesture — clamp on the client if
  you don't want that.

### 6.2 Accessor

Already covered by `ActiveViewState` in §5.3 (`workspace_idx`, `active_workspace_idx`).

### 6.3 Emit — `impl State`

```rust
pub fn ipc_refresh_workspace_switch(&mut self) {
    let Some(server) = &self.niri.ipc_server else { return };
    let Some(v) = self.niri.layout.active_view_state() else { return };

    let mut state = server.event_stream_state.borrow_mut();
    let part = &mut state.workspace_switch;
    let changed = (part.workspace_idx - v.workspace_idx).abs() >= 0.004 // ~250 steps/ws
        || part.active_workspace_idx != v.active_workspace_idx
        || part.output != v.output;
    if !changed { return; }
    part.output = v.output.clone();
    part.workspace_idx = v.workspace_idx;
    part.active_workspace_idx = v.active_workspace_idx;
    part.seen = true;

    server.send_event(Event::WorkspaceSwitchProgress {
        output: v.output,
        workspace_idx: v.workspace_idx,
        active_workspace_idx: v.active_workspace_idx,
    });
}
```

## 7. Client integration (Amane shell)

The client lives in amane: see its
[compositor services](https://github.com/emaki-desktop/amane/blob/emaki/docs/fork/compositor/services.md).
In short, it parses the JSON by key, its socket reader only stores the values and reads on
(or the 64-event buffer fills and niri drops it mid-gesture), and rendering happens on a
separate frame loop.

Put the bar on the **top** layer: in the overview, background/bottom layers zoom with the
workspaces while top/overlay stay put — which is what you want for a bar animating itself.

## 8. Appendix — niri touchpad gesture reference

Gestures niri handles, with whether this spec exposes live progress for them:

| Gesture | Action | Progress exposed | Internal entry points / constants |
|---------|--------|------------------|-----------------------------------|
| 3-finger horizontal swipe | Scroll columns (view offset) | **Yes — signal 2** | `view_offset_gesture_begin/update/end`; `VIEW_GESTURE_WORKING_AREA_MOVEMENT = 1200` |
| 3-finger vertical swipe | Switch workspaces | **Yes — signal 3** | `workspace_switch_gesture_begin/update/end`; `WORKSPACE_GESTURE_MOVEMENT = 300`; rubber-band `{0.5, 0.05}` |
| 4-finger swipe up / down | Open / close overview | **Yes — signal 1** | overview open-close animation/gesture; `OverviewProgress` |
| Overview, 2-finger scroll | Mirrors the 3-finger column/workspace gestures | Yes (same signals 2 & 3) | same gesture state, entered while overview open |
| Touchscreen / tablet drag | Same begin/update/end gesture state machine | Yes (same signals) | `src/input/touch_overview_grab.rs` (`GestureState::ViewOffset`/`WorkspaceSwitch`) |
| DnD edge auto-scroll (view) | Scroll columns while dragging a window near an edge | Yes — rides signal 2 | `dnd_scroll_gesture_begin/scroll/end`; config `gestures.dnd-edge-view-scroll` |
| DnD edge auto-scroll (workspace) | Scroll workspaces while dragging near top/bottom in overview | Yes — rides signal 3 | `dnd_scroll_gesture_*` in monitor.rs; `WORKSPACE_DND_EDGE_SCROLL_MOVEMENT = 1500`; config `gestures.dnd-edge-workspace-switch` |
| Top-left hot corner | Open overview (pointer, not touchpad) | Yes — surfaces as signal 1 | config `gestures.hot-corners` |

Notes:
- DnD edge scroll reuses the *same* `ViewGesture` / `WorkspaceSwitchGesture` state, so it
  needs no extra event — `view_pos` / `workspace_idx` already reflect it.
- niri does not use pinch gestures by default, so there is no pinch progress to expose.
- The horizontal gesture recognizes direction after ~16 px of travel (threshold copied from
  libadwaita) before committing to view-offset vs workspace-switch; until then nothing moves.

### Relevant config blocks (for documentation / testing)

```kdl
gestures {
    hot-corners { /* off to disable */ }
    dnd-edge-view-scroll { trigger-width 30; delay-ms 100; max-speed 1500; }
    dnd-edge-workspace-switch { trigger-height 50; delay-ms 100; max-speed 1500; }
}
input {
    touchpad { /* natural-scroll, scroll-factor, etc. */ }
}
```

## 9. Verification

1. Build the fork, restart niri (don't mix a new `niri msg` with an old compositor).
2. `niri msg --json event-stream` and perform each gesture slowly; confirm the new events
   stream with sane values (overview `0→1`, `view_pos` moving in px, `workspace_idx`
   fractional between integers).
3. Confirm **no** events while idle (coalescing works).
4. Stress: hold a fast continuous swipe for ~10 s with a deliberately slow client; confirm
   the client is *not* dropped (buffer drains) — if it is, move all work off the read loop.
5. Multi-monitor: confirm `output` matches the focused output and switches with focus.
