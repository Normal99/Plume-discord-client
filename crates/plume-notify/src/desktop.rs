use std::fmt;

/// This platform has no notification backend here.
#[derive(Debug)]
pub struct Unsupported;

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("desktop notifications aren't supported on this system")
    }
}

impl std::error::Error for Unsupported {}

/// Shows a notification. `Err` if there's no notification service.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn send(title: &str, body: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::collections::HashMap;
    use std::sync::OnceLock;

    use zbus::blocking::Connection;
    use zbus::zvariant::Value;

    // One session bus connection for the whole run, made on first use.
    static BUS: OnceLock<Option<Connection>> = OnceLock::new();
    let bus = BUS
        .get_or_init(|| Connection::session().ok())
        .as_ref()
        .ok_or(Unsupported)?;

    let hints: HashMap<&str, Value> = HashMap::from([("desktop-entry", Value::from("plume"))]);
    bus.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        // app name, replaces id, icon, summary, body, actions, hints, timeout (-1 = server default)
        &("Plume", 0u32, "", title, body, Vec::<&str>::new(), hints, -1i32),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn send(title: &str, body: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::process::{Command, Stdio};
    // AppleScript string literals: escape backslashes and quotes.
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!("display notification {} with title {}", quote(body), quote(title));
    Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(())
}

#[cfg(not(unix))]
pub fn send(_title: &str, _body: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err(Box::new(Unsupported))
}
