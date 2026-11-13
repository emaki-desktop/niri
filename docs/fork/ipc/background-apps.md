# Background apps: `GetRunningApplications`

Quick Settings' Background Apps tile (Emaki) and GNOME's equivalent read
`org.freedesktop.background.Monitor.BackgroundApps`, which xdg-desktop-portal fills from its
backend's `org.freedesktop.impl.portal.Background.GetAppState`. xdg-desktop-portal-gnome
answers that by calling gnome-shell's `org.gnome.Shell.Introspect.GetRunningApplications`
(`src/background.c`, `get_app_state`). Stock niri's Introspect only had `GetWindows`, so the call
failed ("Could not get window list"), no Flatpak instance was ever classed as background, and
the list stayed empty.

The fork adds, on `org.gnome.Shell.Introspect` at `/org/gnome/Shell/Introspect`:

- **`GetRunningApplications() → a{sa{sv}}`**: one entry per app with a mapped window, keyed
  `<app-id>.desktop` as `GetWindows` names them. Each dict has
  - `sandboxed-app-id` (`s`): the Flatpak app id, when that app's client connected through a
    Flatpak `wp_security_context_v1` (the context's `app_id` with sandbox engine `org.flatpak`,
    kept on the client's `ClientState`);
  - `active-on-seats` (`as`, `["seat0"]`): for the app of the focused window.
- **`RunningApplicationsChanged`** (signal): the backend caches its answer and only asks again on
  this signal. niri emits it when the apps, their sandboxed ids or the focused app change. It
  compares the mapped windows and the focus each loop iteration (a sum over window ids, no
  locks) and reads app ids only when those changed (`State::refresh_running_applications`).

The portal then lists a running Flatpak app with no window as running in the background. **Only
sandboxed (Flatpak) apps can show up**: the portal finds instances through Flatpak, the same as
on GNOME. Two calls overlapping on the one reply channel get an error for the one whose reply
didn't match, never another call's data.

To check, with a Flatpak app running and its window closed:

```sh
busctl --user call org.freedesktop.impl.portal.desktop.gnome /org/freedesktop/portal/desktop \
  org.freedesktop.impl.portal.Background GetAppState
busctl --user get-property org.freedesktop.background.Monitor /org/freedesktop/background/monitor \
  org.freedesktop.background.Monitor BackgroundApps
```
