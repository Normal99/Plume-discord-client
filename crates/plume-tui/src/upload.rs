//! `/upload <file> [message]`: sending files from the input box.

use std::path::{Path, PathBuf};

use plume_rest::MAX_UPLOAD_BYTES;

/// Splits `/upload <file> [message]` into the path and the message. The path
/// may be quoted (terminals often quote dragged-in files) and may start
/// with `~`. Returns `None` if the text isn't an upload command.
pub fn parse(text: &str) -> Option<Result<(PathBuf, String), &'static str>> {
    let rest = text.strip_prefix("/upload")?;
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let rest = rest.trim_start();
    let (path, caption) = match rest.chars().next() {
        None => return Some(Err("Usage: /upload <file> [message]")),
        Some(quote @ ('"' | '\'')) => match rest[1..].find(quote) {
            Some(end) => (&rest[1..=end], &rest[end + 2..]),
            None => return Some(Err("Missing closing quote around the file name")),
        },
        Some(_) => rest.split_once(char::is_whitespace).unwrap_or((rest, "")),
    };
    Some(Ok((expand_home(path), caption.trim().to_owned())))
}

fn expand_home(path: &str) -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    match (path.strip_prefix("~"), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            PathBuf::from(home).join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// Reads a file to upload, refusing ones Discord would reject for size.
pub fn read(path: &Path) -> Result<(String, Vec<u8>), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{} isn't a file", path.display()))?;
    let metadata = std::fs::metadata(path).map_err(|e| format!("Can't open {name}: {e}"))?;
    if !metadata.is_file() {
        return Err(format!("{name} isn't a file"));
    }
    if metadata.len() > MAX_UPLOAD_BYTES as u64 {
        return Err(format!(
            "{name} is {:.1} MB; the limit is {} MB",
            metadata.len() as f64 / 1_048_576.0,
            MAX_UPLOAD_BYTES / 1_048_576
        ));
    }
    let data = std::fs::read(path).map_err(|e| format!("Can't read {name}: {e}"))?;
    Ok((name, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) -> (PathBuf, String) {
        parse(text).unwrap().unwrap()
    }

    #[test]
    fn parses_paths_and_messages() {
        assert!(parse("hello").is_none());
        assert!(parse("/uploader x").is_none());
        assert!(parse("/upload").unwrap().is_err());
        assert_eq!(ok("/upload a.png"), (PathBuf::from("a.png"), String::new()));
        assert_eq!(
            ok("/upload a.png look at this"),
            (PathBuf::from("a.png"), "look at this".into())
        );
        assert_eq!(
            ok("/upload '/tmp/my file.png' hi"),
            (PathBuf::from("/tmp/my file.png"), "hi".into())
        );
        assert_eq!(ok("/upload \"x y\""), (PathBuf::from("x y"), String::new()));
        assert!(parse("/upload 'unclosed").unwrap().is_err());
        assert_eq!(ok("/upload ~nobody/x").0, PathBuf::from("~nobody/x"));
    }

    #[test]
    fn reads_small_files_and_refuses_others() {
        let dir = std::env::temp_dir().join(format!("plume-upload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hi").unwrap();
        assert_eq!(read(&file).unwrap(), ("note.txt".into(), b"hi".to_vec()));
        assert!(read(&dir).unwrap_err().contains("isn't a file"));
        assert!(read(&dir.join("missing")).unwrap_err().contains("Can't open"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
