use std::collections::HashMap;

use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{SerializeDict, Type, Value};

use super::Start;

pub struct Introspect {
    to_niri: calloop::channel::Sender<IntrospectToNiri>,
    from_niri: async_channel::Receiver<NiriToIntrospect>,
}

pub enum IntrospectToNiri {
    GetWindows,
    /// Fork: the apps with a mapped window, for xdg-desktop-portal-gnome's background monitor.
    GetRunningApplications,
}

pub enum NiriToIntrospect {
    Windows(HashMap<u64, WindowProperties>),
    /// Fork: the reply to [`IntrospectToNiri::GetRunningApplications`].
    RunningApplications(HashMap<String, ApplicationProperties>),
}

/// What gnome-shell says about a running app (fork): only the keys xdg-desktop-portal-gnome
/// reads.
#[derive(Debug, Default, Clone, PartialEq, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct ApplicationProperties {
    /// The Flatpak app id of a sandboxed app's client.
    #[zvariant(rename = "sandboxed-app-id")]
    pub sandboxed_app_id: Option<String>,
    /// The seats the app is focused on: `seat0` for the app of the focused window.
    #[zvariant(rename = "active-on-seats")]
    pub active_on_seats: Option<Vec<String>>,
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct WindowProperties {
    /// Window title.
    pub title: String,
    /// Window app ID.
    ///
    /// This is actually the name of the .desktop file, and Shell does internal tracking to match
    /// Wayland app IDs to desktop files. We don't do that yet, which is the reason why
    /// xdg-desktop-portal-gnome's window list is missing icons.
    #[zvariant(rename = "app-id")]
    pub app_id: String,
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetWindows) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::Windows(windows)) => Ok(windows),
            // another call's reply, from calls overlapping on the one channel (fork)
            Ok(_) => Err(fdo::Error::Failed(
                "overlapping calls, try again".to_owned(),
            )),
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    /// The apps with a mapped window, by .desktop id (fork). xdg-desktop-portal-gnome's
    /// background monitor asks this: a running Flatpak app missing here counts as running in
    /// the background. Only sandboxed apps can show up there, as on GNOME.
    async fn get_running_applications(
        &self,
    ) -> fdo::Result<HashMap<String, ApplicationProperties>> {
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetRunningApplications) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::RunningApplications(apps)) => Ok(apps),
            Ok(_) => Err(fdo::Error::Failed(
                "overlapping calls, try again".to_owned(),
            )),
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    /// Fork: the apps, their sandboxed ids or the focused one changed.
    #[zbus(signal)]
    pub async fn running_applications_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;

    // FIXME: call this upon window changes, once more of the infrastructure is there (will be
    // needed for the event stream IPC anyway).
    #[zbus(signal)]
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Introspect {
    pub fn new(
        to_niri: calloop::channel::Sender<IntrospectToNiri>,
        from_niri: async_channel::Receiver<NiriToIntrospect>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::serialized::Context;
    use zbus::zvariant::{to_bytes, Type, LE};

    use super::*;

    #[test]
    fn application_properties_are_a_vardict_without_empty_keys() {
        assert_eq!(ApplicationProperties::SIGNATURE.to_string(), "a{sv}");
        let ctxt = Context::new_dbus(LE, 0);
        let empty = to_bytes(ctxt, &ApplicationProperties::default()).unwrap();
        let full = to_bytes(
            ctxt,
            &ApplicationProperties {
                sandboxed_app_id: Some(String::from("app.zen_browser.zen")),
                active_on_seats: Some(vec![String::from("seat0")]),
            },
        )
        .unwrap();
        // None keys are left out: the same bytes as an empty a{sv}
        let none: HashMap<String, Value<'_>> = HashMap::new();
        assert_eq!(empty.len(), to_bytes(ctxt, &none).unwrap().len());
        assert!(full.len() > empty.len());
    }
}
