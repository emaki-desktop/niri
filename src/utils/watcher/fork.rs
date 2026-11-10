//! The config watcher woken by inotify (fork), instead of polling twice a second.
//!
//! Upstream's watcher checks the config and its includes every 500 ms, which wakes a thread twice
//! a second for the whole session. Here the directories those files live in are watched with
//! inotify, and an event there wakes the watcher for the same check (`WatcherInner::check`): what
//! counts as a change is unchanged, only when it's looked for. The check still runs every
//! [`FALLBACK_INTERVAL`] in case something escaped the watches (a symlinked directory swapped
//! higher up, a bind mount), and the watcher polls as upstream does when inotify isn't there.
//!
//! The watches follow the files: the directory of each path and of where it resolves to, or the
//! nearest one that exists, so a directory made later is seen being made. Directories in the Nix
//! store aren't watched: nothing there changes in place, a new generation is a symlink swapped in
//! a watched directory, and the store itself changes on every build.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use niri_config::ConfigPath;

use super::{WatcherInner, POLLING_INTERVAL};

/// How often the config is checked while inotify watches it, for what the watches can't see.
const FALLBACK_INTERVAL: Duration = Duration::from_secs(10);

/// How long a burst of events (an editor's write, rename and chmod) is let finish before the
/// check, so it's one check and the file is whole.
const SETTLE: Duration = Duration::from_millis(100);

const MASK: u32 = libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_CLOSE_WRITE
    | libc::IN_ATTRIB;

/// What wakes the watcher thread.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Wake {
    /// Load the config, from this path if given: `Watcher::load_config`.
    Load(Option<String>),
    /// Something changed where the config's files are: check them.
    Check,
    /// The `Watcher` was dropped. Upstream's thread ended when the channel closed, but the
    /// inotify thread holds a sender too, so the `Watcher` says so itself.
    Stop,
}

impl Drop for super::Watcher {
    fn drop(&mut self) {
        let _ = self.load_config.send(Wake::Stop);
    }
}

/// The inotify watches on the config's directories.
pub(super) struct FsWatch {
    fd: Option<OwnedFd>,
    watches: HashMap<PathBuf, i32>,
}

impl FsWatch {
    /// Starts watching nothing yet, with a thread that sends [`Wake::Check`] on `wake` for each
    /// burst of events; [`update`](Self::update) sets what's watched. Without inotify, the
    /// watcher polls.
    pub(super) fn new(wake: mpsc::Sender<Wake>) -> Self {
        let fd = match init(wake) {
            Ok(fd) => Some(fd),
            Err(err) => {
                warn!("config watcher: no inotify, polling instead: {err}");
                None
            }
        };
        Self {
            fd,
            watches: HashMap::new(),
        }
    }

    /// How long the watcher waits for a wake before it checks anyway.
    pub(super) fn interval(&self) -> Duration {
        if self.watches.is_empty() {
            POLLING_INTERVAL
        } else {
            FALLBACK_INTERVAL
        }
    }

    /// Watches the directories of the config and its includes as they are now, and stops
    /// watching those no longer in use.
    pub(super) fn update(&mut self, inner: &WatcherInner) {
        let Some(fd) = &self.fd else {
            return;
        };

        let mut paths: Vec<&Path> = match &inner.path {
            ConfigPath::Explicit(path) => vec![path],
            ConfigPath::Regular {
                user_path,
                system_path,
            } => vec![user_path, system_path],
        };
        paths.extend(inner.includes.keys().map(PathBuf::as_path));

        let mut dirs: Vec<PathBuf> = Vec::new();
        for path in paths {
            let resolved = path.canonicalize().ok();
            for path in std::iter::once(path).chain(resolved.as_deref()) {
                if let Some(dir) = existing_dir(path) {
                    if !dir.starts_with("/nix/store") && !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
            }
        }

        self.watches.retain(|dir, wd| {
            let keep = dirs.contains(dir);
            if !keep {
                // SAFETY: fd is an inotify instance and wd one of its watches; a watch the
                // kernel already dropped (its directory deleted) just fails.
                unsafe { libc::inotify_rm_watch(fd.as_raw_fd(), *wd) };
            }
            keep
        });

        for dir in dirs {
            let Ok(c_dir) = CString::new(dir.as_os_str().as_bytes()) else {
                continue;
            };
            // SAFETY: fd is an inotify instance, c_dir a NUL-terminated path. Adding a
            // directory already watched returns its watch again.
            let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), c_dir.as_ptr(), MASK) };
            if wd < 0 {
                debug!(
                    "config watcher: can't watch {dir:?}: {}",
                    std::io::Error::last_os_error()
                );
                self.watches.remove(&dir);
            } else {
                self.watches.insert(dir, wd);
            }
        }
    }
}

/// Makes the inotify instance and the thread reading it.
fn init(wake: mpsc::Sender<Wake>) -> std::io::Result<OwnedFd> {
    // SAFETY: plain syscall; the fd it returns is owned here.
    let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: raw is a new fd nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut reader = File::from(fd.try_clone()?);

    thread::Builder::new()
        .name("Config inotify".to_owned())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(_) => (),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(err) => {
                        warn!("config watcher: inotify read failed: {err}");
                        break;
                    }
                }
                // the rest of the burst queues meanwhile, and comes back as at most one more
                // check
                thread::sleep(SETTLE);
                if wake.send(Wake::Check).is_err() {
                    // the watcher is gone
                    break;
                }
            }
        })?;

    Ok(fd)
}

/// The directory `path` is in, or the nearest one above it that exists.
fn existing_dir(path: &Path) -> Option<PathBuf> {
    let mut dir = path.parent()?;
    loop {
        let candidate = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        if candidate.is_dir() {
            return Some(candidate.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("niri-watcher-fork-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn watched(dir: &Path) -> (FsWatch, mpsc::Receiver<Wake>, PathBuf) {
        let path = dir.join("niri/config.kdl");
        let inner = WatcherInner::new(ConfigPath::Explicit(path.clone()), Vec::new());
        let (tx, rx) = mpsc::channel();
        let mut fs_watch = FsWatch::new(tx);
        fs_watch.update(&inner);
        (fs_watch, rx, path)
    }

    #[test]
    fn a_write_wakes_the_watcher() {
        let dir = temp_dir("write");
        fs::create_dir_all(dir.join("niri")).unwrap();
        fs::write(dir.join("niri/config.kdl"), "a").unwrap();

        let (fs_watch, rx, path) = watched(&dir);
        assert_eq!(fs_watch.interval(), FALLBACK_INTERVAL);
        assert!(rx.try_recv().is_err(), "nothing changed yet");

        fs::write(&path, "b").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Wake::Check));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_made_later_is_seen() {
        let dir = temp_dir("later");

        // niri/ doesn't exist: its parent is watched
        let (mut fs_watch, rx, path) = watched(&dir);
        assert!(fs_watch.watches.contains_key(&dir));

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Wake::Check));

        // after the check, the new directory is watched in its place
        let inner = WatcherInner::new(ConfigPath::Explicit(path.clone()), Vec::new());
        fs_watch.update(&inner);
        assert!(fs_watch.watches.contains_key(&dir.join("niri")));
        assert!(!fs_watch.watches.contains_key(&dir));

        while rx.try_recv().is_ok() {}
        fs::write(&path, "a").unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(Wake::Check));

        fs::remove_dir_all(&dir).unwrap();
    }
}
