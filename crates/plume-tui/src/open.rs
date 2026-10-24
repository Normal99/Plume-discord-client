//! Opening links in the system browser.

use std::io;
use std::process::{Command, Stdio};

/// Opens an http(s) URL with the system handler. Anything else is refused,
/// so a message can't make us run a local file or a custom URL scheme.
pub fn url(url: &str) -> io::Result<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(io::Error::other("only web links can be opened"));
    }
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        // Not `cmd /C start`: cmd would treat `&` in the URL as a command separator.
        Command::new("explorer")
    } else {
        Command::new("xdg-open")
    };
    // Detached and silent: the terminal belongs to the UI.
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_non_web_links() {
        assert!(super::url("file:///etc/passwd").is_err());
        assert!(super::url("javascript:alert(1)").is_err());
    }
}
