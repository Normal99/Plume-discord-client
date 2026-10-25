//! Keeps stray library output off the screen while the UI owns it.
//!
//! Audio libraries (ALSA especially) print warnings straight to stderr,
//! which would scribble over the interface. While the UI runs, stderr goes
//! to the log file instead; it's put back on exit and before a panic
//! message is printed.

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The real stderr, saved while redirected (-1 when not).
    static SAVED: AtomicI32 = AtomicI32::new(-1);

    pub struct Redirect;

    impl Redirect {
        pub fn to_log() -> Option<Redirect> {
            let path = crate::state_dir()?.join("plume-tui.log");
            let log = OpenOptions::new().create(true).append(true).open(path).ok()?;
            // SAFETY: plain fd juggling on descriptors we own or that always exist.
            unsafe {
                let saved = libc::dup(2);
                if saved < 0 || libc::dup2(log.as_raw_fd(), 2) < 0 {
                    return None;
                }
                SAVED.store(saved, Ordering::SeqCst);
            }
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
            Some(Redirect)
        }
    }

    fn restore() {
        let saved = SAVED.swap(-1, Ordering::SeqCst);
        if saved >= 0 {
            // SAFETY: `saved` is the duplicate made in `to_log`.
            unsafe {
                libc::dup2(saved, 2);
                libc::close(saved);
            }
        }
    }

    impl Drop for Redirect {
        fn drop(&mut self) {
            restore();
        }
    }
}

#[cfg(not(unix))]
mod imp {
    /// Windows audio APIs don't print to the console.
    pub struct Redirect;

    impl Redirect {
        pub fn to_log() -> Option<Redirect> {
            Some(Redirect)
        }
    }
}

pub use imp::Redirect;
