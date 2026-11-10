//! The fork's answer to `Request::Features`: what this niri has beyond upstream.
//!
//! A client sends `Features` once and keeps the list. Upstream niri can't parse the request
//! and answers with an error, which is how a client tells it is talking to stock niri. Raise a
//! feature's version when it changes in a way a client can tell; add a name for a new one.
//! docs/fork/ipc/features.md lists what each name covers.

use anyhow::{bail, Context};
use niri_ipc::{Feature, Response};

/// Every fork feature and the version of its contract.
const FEATURES: &[(&str, u32)] = &[
    // Wayland protocols (resources/niri-*.xml); the version is the global's.
    ("blur-shapes", crate::protocols::blur_shapes::VERSION),
    ("vibrancy", crate::protocols::vibrancy::VERSION),
    ("secure-fill", crate::protocols::secure_fill::VERSION),
    // Config and rendering.
    ("materials", 1),
    ("mat", 1),
    ("desktop-layer", 1),
    ("hdr", 1),
    // IPC additions.
    ("move-column-to-index-window-id", 1),
    ("window-fullscreen", 1),
    ("overview-progress", 1),
    ("view-offset", 1),
    ("workspace-switch-progress", 1),
    ("desktop-obscured", 1),
    ("lock-keys", 1),
    ("outputs-changed", 1),
    ("wallpaper-dim", 1),
];

/// The list sent in `Response::Features`.
pub fn list() -> Vec<Feature> {
    FEATURES
        .iter()
        .map(|&(name, version)| Feature {
            name: name.to_owned(),
            version,
        })
        .collect()
}

/// Prints the reply to `niri msg features`.
pub fn print(response: Response, json: bool) -> anyhow::Result<()> {
    let Response::Features(features) = response else {
        bail!("unexpected response: expected Features, got {response:?}");
    };

    if json {
        let features = serde_json::to_string(&features).context("error formatting response")?;
        println!("{features}");
        return Ok(());
    }

    for Feature { name, version } in features {
        println!("{name} {version}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use niri_ipc::Request;

    use super::*;

    #[test]
    fn names_are_unique() {
        let names: HashSet<_> = FEATURES.iter().map(|(name, _)| name).collect();
        assert_eq!(names.len(), FEATURES.len());
    }

    #[test]
    fn request_and_reply_round_trip() {
        let request = serde_json::to_string(&Request::Features).unwrap();
        assert_eq!(request, r#""Features""#);

        let reply: niri_ipc::Reply = Ok(Response::Features(list()));
        let text = serde_json::to_string(&reply).unwrap();
        assert!(text.starts_with(r#"{"Ok":{"Features":[{"name":"blur-shapes","version":2}"#));

        let back: niri_ipc::Reply = serde_json::from_str(&text).unwrap();
        let Ok(Response::Features(features)) = back else {
            panic!("expected Features, got {back:?}");
        };
        assert_eq!(features, list());
    }
}
