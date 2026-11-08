# Maintaining the fork

How the fork is kept on top of upstream niri: provenance, branches, the doc rule, the rebase
workflow and every fork commit. The features themselves are in the [index](README.md).

## Provenance

| | |
|---|---|
| Upstream | `https://github.com/niri-wm/niri` (remote: `upstream`; it moved there from `YaLTeR/niri`, which redirects) |
| Fork | `https://github.com/098765434/niri` (remote: `origin`) |
| Base release | **`v26.04`** (matches the niri 26.04 that nixpkgs 26.05 ships), tagged `fork-base-v26.04` |
| Fork branch | `ocbk` |
| The delta | `git log --oneline fork-base-v26.04..ocbk` |

## Branches

| Branch | State |
|---|---|
| `ocbk` | the fork; everything below is on it |
| `ocbk-pre-tidy` | history: `ocbk` before the 2026-11-07 history tidy (`16ab6bdb`), kept until a session has run on the new one |
| `pinned-columns` | experiment, not merged: pushed (`99562604`, based before the docs moved); see [below](#pinned-columns) |
| `mod-swipe-gestures` | experiment, not merged as a branch: local only, ends at `6d9306a7` (a commit of `ocbk-pre-tidy`); its work is on `ocbk` as `823b718a` |
| `origin/gesture-ipc-events` | the branch's old name, before it was renamed to `ocbk`; stale (ends at `c5b8c69d`) |

The other `origin/*` branches are upstream's, mirrored by the GitHub fork.

### Pinned columns

A column docked to a screen edge, with its own rail, outside the scroll math; actions
`pin-column`, `unpin-column`, `toggle-column-pinned`, `focus-pinned`. The compositor core is done
with tests; the overview drop target, shell UI and cross-workspace pins are deferred.

The branch predates `docs/fork/`. When it is rebased onto `ocbk`:

- its root `niri-pinned-columns-spec.md` becomes `docs/fork/layout/pinned-columns.md`, with
  Usage first like the other feature docs, and a row in the [index](README.md);
- its `FORK.md` has the old layout (a long feature list) and will conflict: keep `ocbk`'s stub
  and move the pinned-columns notes into the new doc.

## Docs rule

Upstream files stay byte-identical to `v26.04`, with the exceptions `FORK.md` names: the
one-line banner at the top of `README.md`, the `v0_2` feature of `libdisplay-info` in
`Cargo.toml`, and `.claude/` in `.gitignore`. Everything else the fork adds is in new files
(`FORK.md`, `docs/fork/`, `resources/niri-*-v1.xml`, `.github/workflows/fork.yml`) or in
hooks inside upstream's sources. All fork documentation lives in `docs/fork/`, so this prints
nothing:

```sh
git diff fork-base-v26.04 HEAD -- docs/wiki
```

and a rebase only ever conflicts on the README banner line and the `Cargo.toml` line, never on
a wiki page.
New user-facing docs for a fork feature go in that feature's doc under `docs/fork/`, as its
Usage section, not into `docs/wiki/`. Upstream's `niri-config/tests/wiki-parses.rs` checks the
wiki's ```` ```kdl ```` blocks; the fork's own `niri-config/tests/fork-docs-parse.rs` does the same
for every block under `docs/fork/` (mark one ```` ```kdl must-fail ```` if it should not parse).

Specs that are implemented go in `docs/fork/design/` as history, with a
`Status: implemented in <hash>` line.

## Reviewing the fork

```sh
git log --oneline --stat fork-base-v26.04..ocbk   # every fork commit and the files it touched
git diff --stat fork-base-v26.04..ocbk            # the whole delta, by file
git diff fork-base-v26.04..ocbk -- <paths>        # one feature (paths in each feature doc)
git range-diff <old-base> <new-base> ocbk # after a rebase: what changed in each commit
```

## Consumed by

Built as the compositor via a flake input (`programs.niri.package`), e.g. in a NixOS config:

```nix
niri-fork = { url = "github:098765434/niri/ocbk"; inputs.nixpkgs.follows = "nixpkgs"; };
```

Commit on this branch, then `nix flake update niri-fork` and rebuild.

## Updating to a new upstream release

The fork's commits sit on a release tag, and `fork-base-<tag>` marks the tag the current
`ocbk` is based on (`fork-base-v26.04` today). Updating is a rebase onto the next tag:

1. **Tag the new base.** Upstream is `https://github.com/niri-wm/niri.git` (check with
   `git remote get-url upstream`; the old `YaLTeR/niri` URL redirects, but set the new one).
   ```sh
   git fetch --tags upstream
   git tag fork-base-<new-tag> <new-tag>
   git branch ocbk-pre-<new-tag> ocbk     # the way back, until a session has run on the result
   ```
2. **Rebase.**
   ```sh
   git rebase --onto fork-base-<new-tag> fork-base-v26.04 ocbk
   ```
   Conflicts land mostly in the hot files below. Keep upstream's side and re-apply the fork's
   one-line hook; the logic lives in the fork's own files. For a commit upstream already has,
   `git rebase --skip`.
3. **Read the range-diff.**
   ```sh
   git range-diff fork-base-v26.04..ocbk-pre-<new-tag> fork-base-<new-tag>..ocbk
   ```
   Every commit should match its old self apart from conflict fixes. A commit that disappeared
   was either upstreamed or dropped by mistake.
4. **Build and test** in the dev shell: `cargo build`, `cargo test --all --exclude
   niri-visual-tests -- --skip=::egl` (it includes `fork-docs-parse`), `cargo clippy --all
   --all-targets`. Then check [what amane-shell relies on](#what-amane-shell-relies-on) still holds:
   amane-shell's `cargo xtask contract` against the new revision.
5. **Run a session** on it: `nix flake update niri-fork` in the consuming config, switch, log in
   again, and check HDR, glass, the mat, the desktop layer and the swipes by eye.

Then replace `v26.04` with the new tag here, in [the index](README.md) and in `FORK.md`, push
`ocbk` with `--force-with-lease`, push the `fork-base-<new-tag>` tag, and delete
`ocbk-pre-<new-tag>` once the session looks right.

### Hot upstream files

The fork keeps its logic in its own files and leaves short hooks in upstream's, so a conflict
there is a line to re-add. Fork hunks against `v26.04` (`git diff fork-base-v26.04 -U0 -- <file> | grep -c
'^@@'`):

| File | Hunks | Lines added | Where the logic lives |
|---|---|---|---|
| `src/backend/tty.rs` | 31 (36 before `4e448827`) | 122 (374 before) | `src/backend/tty/hdr.rs` |
| `src/niri.rs` | 38 (38 before `7f180178`) | 103 (163 before) | `src/niri/fork.rs` |
| `src/render_helpers/framebuffer_effect.rs` | 20 | 486 | in place: mostly new types and functions appended, so they rarely conflict |
| `src/render_helpers/background_effect.rs` | 25 | 320 | in place, the same |
| `src/render_helpers/xray.rs` | 21 | 220 | in place: `compute_uniforms` and buffer release are reworked, so upstream changes to xray conflict here |

What stays in `tty.rs`: the HDR fields on `Surface`, the 10-bit retry when the DRM compositor
can't be created (it is threaded through upstream's invalid-modifier retry), and the HDR fields of
the IPC output. In `niri.rs`: the desktop layer's hit-testing, which is inline in upstream's
closures, and the render order of desktop layers.

### Porting the protocols to Smithay's next dispatch API

Smithay is reworking how protocol objects are dispatched. When the upstream tag that needs it
arrives, each fork protocol moves over as a unit; nothing outside its file and its `delegate_*!`
line changes. What each one has today:

| Protocol | File | Global | Objects dispatched | Delegated in |
|---|---|---|---|---|
| `niri-blur-shapes-v1` | `src/protocols/blur_shapes.rs` | `niri_blur_shapes_manager_v1` (no global data) | manager, `niri_blur_shapes_v1` (`BlurShapesUserData`) | `src/handlers/background_effect.rs` |
| `niri-vibrancy-v1` | `src/protocols/vibrancy.rs` | `niri_vibrancy_manager_v1` (no global data) | manager, `niri_vibrancy_v1` (`VibrancyUserData`) | `src/handlers/background_effect.rs` |
| `niri-secure-fill-v1` | `src/protocols/secure_fill.rs` | `niri_secure_fill_manager_v1` (`SecureFillGlobalData`: the client filter) | manager, `niri_secure_fill_target_v1` (`TargetData`) | `src/handlers/mod.rs` |
| `wp-color-management-v1` | `src/protocols/color_management.rs` | `wp_color_manager_v1` | manager, output, surface, surface feedback, parametric and ICC creators, image description, its info, and its reference: 9 object types | `src/handlers/background_effect.rs` |

For each: port the `GlobalDispatch` and `Dispatch` impls to the new traits, port the
`delegate_*!` macro the same way upstream's protocols in `src/protocols/` are ported in that
tag (follow one of theirs, such as `ext_workspace.rs`), and keep the `*Handler` trait that
`State` implements unchanged. Color management is the largest, and its `send_pending_information`
(replies that destroy the object they're on, sent after dispatch) is the part to check against
the new API's object lifetimes. Then run the protocol tests in `src/tests/` (color management
has its own).

## What amane-shell relies on

amane-shell (through the amane library) uses these. A rebase keeps them, or bumps the feature's
version in `src/ipc/features.rs` and says so in amane-shell's `docs/integration/contract.md`.
Stock niri has none of them; a client asks [`Features`](ipc/features.md) first.

| Kind | What | Feature |
|---|---|---|
| Wayland protocol | `niri-blur-shapes-v1` version 2 (`resources/niri-blur-shapes-v1.xml`) | `blur-shapes 2` |
| Wayland protocol | `niri-vibrancy-v1` version 1 | `vibrancy 1` |
| Wayland protocol | `niri-secure-fill-v1` version 1 | `secure-fill 1` |
| Wayland protocol | `wp-color-management-v1` (stock protocol) | `hdr 1` |
| IPC request | `Features` | — |
| IPC event | `OverviewProgress`, `ViewOffsetChanged`, `WorkspaceSwitchProgress` | `overview-progress`, `view-offset`, `workspace-switch-progress` |
| IPC event | `DesktopObscured` | `desktop-obscured 1` |
| IPC event | `KeyboardLockKeysChanged` | `lock-keys 1` |
| IPC event | `OutputsChanged` | `outputs-changed 1` |
| IPC field | `Window.is_fullscreen` | `window-fullscreen 1` |
| IPC field | `Output.hdr_supported`, `hdr_enabled`, `sdr_brightness`, `peak_luminance` | `hdr 1` |
| IPC action | `MoveColumnToIndex { window_id }` | `move-column-to-index-window-id 1` |
| IPC action | `Output { action: Hdr }`, `Output { action: SdrBrightness }` | `hdr 1` |
| Config | `materials {}`, `background-effect { material }` in window and layer rules | `materials 1` |
| Config | `layout { mat {} }` | `mat 1` |
| Config | `layer-rule { desktop true }` | `desktop-layer 1` |
| Config | `output { hdr; sdr-brightness }` | `hdr 1` |
| Config | `layout { workspace-gap }`, `overview { workspace-gap window-gap }` | none: config only |

The protocol XMLs are copied byte for byte into amane's `protocols/`; amane-shell's `cargo xtask
contract` checks the copies match at the pinned revisions, and that every IPC name amane sends
or parses exists in `niri-ipc` here.

## Upstreaming

| Feature | Upstreamable? |
|---|---|
| IPC events | maybe: generic IPC, but per-frame events are chatty |
| IPC actions | likely: small, optional, and matches `MoveWindowToWorkspace`'s `window_id` |
| Mod swipe | maybe: behind a config switch |
| Blur shapes, materials | no as-is: private protocol; GPL-3.0 shader lineage (kwin-effects-glass, Niri-glass) |
| Vibrancy | no as-is: private protocol |
| Gaps | maybe: plain config, stock behaviour unchanged when unset |
| Workspace mat | maybe: plain config, no protocol |
| Desktop layer | maybe: plain config plus one IPC event |
| HDR | maybe in parts: the color-management protocol and conversion are generic; the composition keeps the fork's gamma-2.2 blending |
| `Window.is_fullscreen` | likely: one IPC field, defaulted for older clients |
| Secure fill | no as-is: private protocol, for one password manager's fill |
| `Features` | no: it lists this fork's features |

## Commit log

Every commit in `fork-base-v26.04..ocbk`, oldest first. On 2026-11-07 the history was tidied: fixups were
squashed into the commits they fix, each feature's docs went into its commit, edits to
`docs/wiki` and their later reset were dropped, and nothing in the final tree changed apart from a
comment. The branch before that is `ocbk-pre-tidy` (`16ab6bdb`).

| Commit | Feature | What |
|---|---|---|
| `759c9c3f` | [IPC events](ipc/events.md) | live gesture-progress IPC events |
| `3e821f3d` | housekeeping | add `FORK.md`; ignore `.claude/` |
| `44065213` | housekeeping | README fork banner; rebase/range-diff workflow |
| `1a397bc0` | housekeeping | rename the work branch to `ocbk` |
| `bf1916c7` | housekeeping | `FORK.md` update |
| `823b718a` | [mod swipe](gestures/mod-swipe.md) | Mod-held three-finger window swipes, stepping into full width past the widest preset, with its design doc |
| `113728ce` | [blur shapes](effects/blur-shapes.md) | `niri-blur-shapes-v1`: rounded, fading blur shapes |
| `e843c4be` | [materials](effects/materials.md) | glass materials for blur shapes |
| `39904a0b` | [blur shapes](effects/blur-shapes.md) | magnify inwards at the lip; one element id per blur shape |
| `e6ee524a` | housekeeping | feature table in `FORK.md`, design docs in `docs/fork` |
| `3a984abc` | [IPC actions](ipc/actions.md) | `MoveColumnToIndex` takes an optional `window_id` |
| `379f1071` | [materials](effects/materials.md) | materials on rules, materializing lenses, backdrop readings |
| `14ed7262` | [vibrancy](effects/vibrancy.md) | `niri-vibrancy-v1`: blend a surface with what's behind it |
| `9cdf1c8d` | [vibrancy](effects/vibrancy.md) | plus-darker subtracts the backdrop |
| `c0c39971` | [materials](effects/materials.md) | a blur of their own (`blur-passes`, `blur-offset`) |
| `bf313b9b` | [materials](effects/materials.md) | `tone`, a luminosity adjustment that keeps thin tints readable |
| `5f986fdf` | [materials](effects/materials.md) | outward lens with a capture reach, and `glow` apart from the rim |
| `7e622e2d` | [workspace mat](layout/workspace-mat.md) | a workspace mat behind the scrolling layout, with its spec |
| `3696f762` | [workspace mat](layout/workspace-mat.md) | soft ends fade out inside the end columns, and for glass too |
| `9165052c` | [workspace mat](layout/workspace-mat.md) | `behind-bars` |
| `9e57324e` | [gaps](layout/gaps.md) | `workspace-gap`, overview `workspace-gap` and `window-gap` |
| `f4e61d28` | [gaps](layout/gaps.md) | measure outside the bars' exclusive zones, allow negatives |
| `193f7663` | [gaps](layout/gaps.md) | step by the rounded working-area edges |
| `12509ec6` | [desktop layer](layout/desktop-layer.md) | `desktop` layer rule and the `DesktopObscured` event |
| `9f33ce72` to `8b819f28` | housekeeping | docs reorganization: `docs/fork/` feature folders, the wiki sections moved there, the `FORK.md` stub, this file and the index |
| `12525c21` | housekeeping | `fork-docs-parse`: the KDL blocks in `docs/fork` parse |
| `7874959a` | [effects memory](effects/memory.md) | base chain and background only when used, idle eviction, capture targets released, shape captures dropped, swipe offscreen cleared |
| `ae549528` | [HDR](display/hdr.md) | HDR10 output: KMS properties, 10-bit scanout, half-float composition encoded to PQ |
| `ddd3381d` | [HDR](display/hdr.md) | `wp-color-management-v1`, HDR surfaces converted and tone-mapped per target; no crash on an image description's information |
| `26290100` | [IPC events](ipc/events.md) | `Window.is_fullscreen`, read before taking the toplevel's lock |
| `85a7be33` | [secure fill](input/secure-fill.md) | `niri-secure-fill-v1` (`resources/niri-secure-fill-v1.xml`): fill a text field only if it's still the one checked |
| `2792bc5a` | housekeeping | the fork's CI (`.github/workflows/fork.yml`), in the flake's dev shell, with the terms check; upstream's `ci.yml` can't build the fork, since HDR needs libdisplay-info 0.2 |
| `8f8275e6` | housekeeping | docs: `is_fullscreen`, secure fill's commit and protocol, the commit log |
| `bebe757d` | [features](ipc/features.md) | `Request::Features` and `niri msg features` |
| `bd862d07` | [IPC events](ipc/events.md) | `DesktopObscured` forgets unplugged outputs; tests for the fork's event-stream state |
| `7f180178` | housekeeping | `Niri`'s fork parts in `src/niri/fork.rs` |
| `4e448827` | [HDR](display/hdr.md) | HDR on a TTY output in `src/backend/tty/hdr.rs` |
| `989d475e` | [IPC events](ipc/events.md) | `KeyboardLockKeysChanged` and `OutputsChanged` (`src/ipc/server/fork.rs`) |
| `f8c4e94e`, `dc3e72c7`, `d42a83ad` | housekeeping | docs: the commit log, the index's event and protocol names, the mod-swipe and mat call flows as Mermaid |
| `a575e7c5` | [HDR](display/hdr.md) | `hdr-auto`: HDR only while something on the output is HDR |
| `54bcc56a` | [effects memory](effects/memory.md) | a change re-blurs an xray buffer only near where it's sampled |
| `cfece120`, `d0563d47`, `0cf3ec8e` | housekeeping | docs: upstream moved to `niri-wm/niri`; `FORK.md` as a fork notice; the index in two parts |
