//! Plume's 🪶 Feather client: Discord in the terminal, in a few MB of RAM.
//!
//!     plume-tui                 start the client
//!     plume-tui login           log in (QR code)
//!     plume-tui login --token   log in with $DISCORD_TOKEN
//!     plume-tui logout          forget the saved token

mod app;
mod input;
mod layout;
mod login;
mod open;
mod render;
mod sidebar;
mod text;

use std::path::PathBuf;

use plume_core::Profile;

const HELP: &str = "\
plume-tui: a lightweight Discord client for the terminal

Usage:
  plume-tui [options]       start the client
  plume-tui login           log in by scanning a QR code
  plume-tui login --token   log in with the token in $DISCORD_TOKEN
  plume-tui logout          remove the saved token

Options:
  --profile <name>   feather (default), standard or full: how many
                     channels and messages to keep in memory
  --silent-typing    don't tell others when you're typing
  --no-notifications no desktop notifications for DMs and mentions
  --help             show this help

Logs go to ~/.local/state/plume/plume-tui.log (set RUST_LOG=debug for more).";

/// Settings from the command line.
pub struct Options {
    pub profile: Profile,
    pub silent_typing: bool,
    pub notifications: bool,
}

// One thread is plenty for a chat client and keeps memory low (Feather).
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error}");
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
        _ => {}
    }

    let options = parse_options(&args)?;
    init_logging();

    let token = match plume_auth::token_from_env() {
        Some(token) => token,
        None => tokio::task::spawn_blocking(plume_auth::keyring::load_token)
            .await??
            .ok_or("not logged in yet. Run `plume-tui login` first")?,
    };
    app::run(token, options).await
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        profile: Profile::FEATHER,
        silent_typing: false,
        notifications: true,
    };
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
            other => return Err(format!("unknown option `{other}`. See --help")),
        }
    }
    Ok(options)
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
