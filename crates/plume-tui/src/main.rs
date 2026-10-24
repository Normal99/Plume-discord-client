//! Plume's 🪶 Feather client: Discord in the terminal, in a few MB of RAM.
//!
//!     plume-tui                 start the client
//!     plume-tui login           log in (QR code)
//!     plume-tui login --token   log in with $DISCORD_TOKEN
//!     plume-tui logout          forget the saved token
//!     plume-tui config          show or create the settings file

mod app;
mod config;
mod input;
mod layout;
mod login;
mod open;
mod render;
mod sidebar;
mod text;
mod upload;

use std::path::PathBuf;

use plume_core::Profile;

const HELP: &str = "\
plume-tui: a lightweight Discord client for the terminal

Usage:
  plume-tui [options]       start the client
  plume-tui login           log in by scanning a QR code
  plume-tui login --token   log in with the token in $DISCORD_TOKEN
  plume-tui logout          remove the saved token
  plume-tui config          show the settings file (creates it if missing)

Options:
  --profile <name>   feather (default), standard or full: how many
                     channels and messages to keep in memory
  --silent-typing    don't tell others when you're typing
  --no-notifications no desktop notifications for DMs and mentions
  --show-locked      also list channels you can't read (toggle with L)
  --help             show this help

Settings can also go in the config file (see `plume-tui config`);
command-line options win.

Logs go to ~/.local/state/plume/plume-tui.log (set RUST_LOG=debug for more).";

/// Settings from the config file and the command line.
#[derive(Debug, PartialEq)]
pub struct Options {
    pub profile: Profile,
    pub silent_typing: bool,
    pub notifications: bool,
    pub show_locked: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            profile: Profile::FEATHER,
            silent_typing: false,
            notifications: true,
            show_locked: false,
        }
    }
}

// One thread is plenty for a chat client and keeps memory low (Feather).
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        // Started by double-clicking, the window would vanish before the
        // error could be read.
        if cfg!(windows) {
            eprintln!("Press Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return Ok(());
    }
    match args.first().map(String::as_str) {
        Some("login") => return login::login(args.iter().any(|a| a == "--token")).await,
        Some("logout") => return login::logout().await,
        Some("config") => return show_config(),
        _ => {}
    }

    let mut options = Options::default();
    config::load(&mut options)?;
    parse_options(&args, &mut options)?;
    init_logging();

    let token = match plume_auth::token_from_env() {
        Some(token) => token,
        None => match tokio::task::spawn_blocking(plume_auth::keyring::load_token).await?? {
            Some(token) => token,
            None => {
                // First run: log in, then carry on into the client.
                println!("You're not logged in yet, so let's do that first.\n");
                login::login(false).await?;
                tokio::task::spawn_blocking(plume_auth::keyring::load_token)
                    .await??
                    .ok_or("the login didn't save a token")?
            }
        },
    };
    app::run(token, options).await
}

/// Prints where the config file is, creating it from the example if needed.
fn show_config() -> Result<(), Box<dyn std::error::Error>> {
    let path = config::path().ok_or("can't find your config folder")?;
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, config::EXAMPLE)?;
        println!("Created {}:\n", path.display());
    } else {
        println!("{}:\n", path.display());
    }
    print!("{}", std::fs::read_to_string(&path)?);
    Ok(())
}

fn parse_options(args: &[String], options: &mut Options) -> Result<(), String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--profile" => {
                let name = args.next().ok_or("--profile needs a name")?;
                options.profile = Profile::from_name(name)
                    .ok_or_else(|| format!("unknown profile `{name}` (try feather, standard or full)"))?;
            }
            "--silent-typing" => options.silent_typing = true,
            "--no-notifications" => options.notifications = false,
            "--show-locked" => options.show_locked = true,
            other => return Err(format!("unknown option `{other}`. See --help")),
        }
    }
    Ok(())
}

/// The terminal belongs to the UI, so logs go to a file instead.
fn init_logging() {
    let Some(dir) = state_dir() else { return };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(file) = std::fs::File::create(dir.join("plume-tui.log")) else {
        return;
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .init();
}

/// `$XDG_STATE_HOME/plume`, falling back to `~/.local/state/plume`
/// (`%LOCALAPPDATA%\plume` on Windows).
fn state_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("plume"));
    }
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/state"),
    };
    Some(base.join("plume"))
}
