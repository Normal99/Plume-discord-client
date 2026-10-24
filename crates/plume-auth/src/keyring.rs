//! Token storage. The OS keyring (Secret Service on Linux, Keychain on
//! macOS, Credential Manager on Windows) is used whenever it works. Some
//! systems have none (servers, minimal Linux installs, WSL), and there the
//! token goes in a file only you can read, `~/.config/plume/token`.
//!
//! These calls block briefly while talking to the keyring service. From async
//! code, run them with `tokio::task::spawn_blocking`.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use keyring::{Entry, Error};
use plume_core::Token;
use tracing::warn;

use crate::AuthError;

const SERVICE: &str = "plume";
const ACCOUNT: &str = "discord-token";

/// Where a token was saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    Keyring,
    /// No keyring was available, so it's in this file (readable only by you).
    File(PathBuf),
}

fn entry() -> Result<Entry, AuthError> {
    Ok(Entry::new(SERVICE, ACCOUNT)?)
}

/// The saved token, or `None` if nobody has logged in yet.
pub fn load_token() -> Result<Option<Token>, AuthError> {
    let from_keyring = entry().and_then(|entry| match entry.get_password() {
        Ok(token) => Ok(Some(Token::new(token))),
        Err(Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    });
    match from_keyring {
        Ok(Some(token)) => Ok(Some(token)),
        Ok(None) => load_file(),
        Err(error) => {
            warn!("keyring unavailable ({error}); checking the token file");
            load_file()
        }
    }
}

/// Saves the token in the keyring, or in the token file if there's no keyring.
pub fn save_token(token: &Token) -> Result<Storage, AuthError> {
    match entry().and_then(|entry| Ok(entry.set_password(token.expose())?)) {
        Ok(()) => {
            // An old file copy would shadow nothing, but it shouldn't linger.
            let _ = remove_file();
            Ok(Storage::Keyring)
        }
        Err(error) => {
            warn!("keyring unavailable ({error}); saving the token to a file");
            save_file(token).map(Storage::File)
        }
    }
}

/// Removes the saved token, wherever it is. Not an error if there wasn't one.
pub fn delete_token() -> Result<(), AuthError> {
    let keyring = entry().and_then(|entry| match entry.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    });
    remove_file()?;
    if let Err(error) = keyring {
        // No keyring at all means there's nothing in it to delete.
        warn!("couldn't reach the keyring to remove the token: {error}");
    }
    Ok(())
}

/// `$XDG_CONFIG_HOME/plume/token`, `~/.config/plume/token`, or
/// `%APPDATA%\plume\token` on Windows.
pub fn token_file() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        }
    };
    Some(base.join("plume").join("token"))
}

fn load_file() -> Result<Option<Token>, AuthError> {
    let Some(path) = token_file() else { return Ok(None) };
    match fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => Ok(Some(Token::new(text))),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AuthError::TokenFile(error)),
    }
}

fn save_file(token: &Token) -> Result<PathBuf, AuthError> {
    let path = token_file()
        .ok_or_else(|| AuthError::TokenFile(std::io::Error::other("no home directory to save the token in")))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(AuthError::TokenFile)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(AuthError::TokenFile)?;
    // `mode` only applies to new files; tighten an existing one too.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(AuthError::TokenFile)?;
    }
    file.write_all(token.expose().as_bytes())
        .map_err(AuthError::TokenFile)?;
    Ok(path)
}

fn remove_file() -> Result<(), AuthError> {
    let Some(path) = token_file() else { return Ok(()) };
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AuthError::TokenFile(error)),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn token_file_round_trip_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("plume-token-test-{}", std::process::id()));
        // SAFETY: this is the only test in the crate touching the environment.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        let path = save_file(&Token::new("abc.def")).unwrap();
        assert_eq!(path, dir.join("plume/token"));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load_file().unwrap().unwrap().expose(), "abc.def");
        remove_file().unwrap();
        assert!(load_file().unwrap().is_none());
        let _ = fs::remove_dir_all(dir);
    }
}
