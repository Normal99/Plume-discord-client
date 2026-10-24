//! The optional settings file, `config.toml` in Plume's config folder.
//!
//! Only flat `key = value` lines are needed, so this reads that subset of
//! TOML by hand instead of pulling in a parser.

use std::path::PathBuf;

use plume_core::Profile;

use crate::Options;

/// What a new config file looks like; also shown by `plume-tui config`.
pub const EXAMPLE: &str = "\
# Plume settings. Command-line options override these.

# feather, standard or full: how many channels and messages to keep in memory
profile = \"feather\"
# Don't tell others when you're typing
silent_typing = false
# Desktop notifications for DMs and mentions
notifications = true
# Also list channels you can't read (toggle with L in the sidebar)
show_locked = false
";

pub fn path() -> Option<PathBuf> {
    Some(plume_auth::keyring::config_dir()?.join("config.toml"))
}

/// Reads the config file into `options`. A missing file is fine.
pub fn load(options: &mut Options) -> Result<(), String> {
    let Some(path) = path() else { return Ok(()) };
    match std::fs::read_to_string(&path) {
        Ok(text) => apply(&text, options).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

fn apply(text: &str, options: &mut Options) -> Result<(), String> {
    for (number, line) in text.lines().enumerate() {
        let line = line.split_once('#').map_or(line, |(code, _)| code).trim();
        if line.is_empty() {
            continue;
        }
        let at = |message: String| format!("line {}: {message}", number + 1);
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| at(format!("expected `key = value`, got `{line}`")))?;
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        let flag = || match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(at(format!("`{key}` should be true or false"))),
        };
        match key {
            "profile" => {
                options.profile = Profile::from_name(value)
                    .ok_or_else(|| at(format!("unknown profile `{value}` (try feather, standard or full)")))?;
            }
            "silent_typing" => options.silent_typing = flag()?,
            "notifications" => options.notifications = flag()?,
            "show_locked" => options.show_locked = flag()?,
            _ => return Err(at(format!("unknown setting `{key}`"))),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_to_the_defaults() {
        let mut options = Options::default();
        apply(EXAMPLE, &mut options).unwrap();
        assert_eq!(options, Options::default());
    }

    #[test]
    fn reads_settings_and_reports_mistakes() {
        let mut options = Options::default();
        apply("profile = full  # lots of RAM\nshow_locked=true\n\n", &mut options).unwrap();
        assert_eq!(options.profile, Profile::FULL);
        assert!(options.show_locked);

        let error = apply("notifications = maybe", &mut options).unwrap_err();
        assert_eq!(error, "line 1: `notifications` should be true or false");
        assert!(
            apply("\ncolour = red", &mut options)
                .unwrap_err()
                .starts_with("line 2: unknown setting")
        );
        assert!(apply("just words", &mut options).is_err());
    }
}
