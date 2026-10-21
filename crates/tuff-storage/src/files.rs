//! Whole NBT files on disk, written the way vanilla replaces them.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use tuff_nbt::{Compound, Compression, NamedTag, NbtLimits, Tag};

/// Upper bound on any decompressed NBT file read from the world (level.dat,
/// saved data, player data). Generous enough for heavily used player files.
pub const MAX_FILE_NBT_BYTES: usize = 32 * 1024 * 1024;

/// Replaces `target` the way vanilla's `Util.safeReplaceFile` does: the new
/// contents go to a temporary file first, the previous file is kept as
/// `<target>_old`, and the temporary file is renamed into place.
pub fn safe_replace(target: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = target.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("data");
    let temp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    {
        let mut file = File::create(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    if target.exists() {
        let backup = dir.join(format!("{name}_old"));
        fs::copy(target, &backup)?;
    }
    fs::rename(&temp, target)
}

/// Reads a gzip-compressed NBT file's root compound. A missing file is `None`.
pub fn read_nbt_file(path: &Path) -> io::Result<Option<Compound>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let limits = NbtLimits {
        max_bytes: MAX_FILE_NBT_BYTES,
        ..NbtLimits::TRUSTED
    };
    let (named, _) = NamedTag::from_bytes(&data, limits).map_err(io::Error::other)?;
    match named.tag {
        Tag::Compound(root) => Ok(Some(root)),
        _ => Err(io::Error::other(format!(
            "{} does not contain a compound",
            path.display()
        ))),
    }
}

/// Writes `root` as a gzip-compressed, unnamed-root NBT file via
/// [`safe_replace`].
pub fn write_nbt_file(path: &Path, root: Compound) -> io::Result<()> {
    let bytes = NamedTag::new("", root)
        .to_bytes(Compression::Gzip)
        .map_err(io::Error::other)?;
    safe_replace(path, &bytes)
}

const MAX_FILE_NAME: usize = 255;
const ILLEGAL_FILE_CHARACTERS: [char; 15] = [
    '/', '\n', '\r', '\t', '\0', '\u{c}', '`', '?', '*', '\\', '<', '>', '|', '"', ':',
];

/// Replaces characters that are illegal in file names on some platform.
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if ILLEGAL_FILE_CHARACTERS.contains(&c) || matches!(c, '.' | '"') {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Whether a name avoids Windows reserved device names and a trailing dot.
pub fn is_path_part_portable(name: &str) -> bool {
    if name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "COM" | "CLOCK$" | "CON" | "PRN" | "AUX" | "NUL"
    ) || ((stem.len() == 4)
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    !reserved
}

fn truncate_chars(text: &str, max: usize) -> &str {
    text.char_indices()
        .nth(max)
        .map_or(text, |(index, _)| &text[..index])
}

fn split_copy_counter(name: &str) -> Option<(&str, u32)> {
    let inner = name.strip_suffix(')')?;
    let open = inner.rfind(" (")?;
    let digits = &inner[open + 2..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((&inner[..open], digits.parse().ok()?))
}

/// Picks a directory name under `base_dir` that does not exist yet, as
/// `name`, `name (1)`, `name (2)` and so on, followed by `suffix`.
pub fn find_available_name(base_dir: &Path, name: &str, suffix: &str) -> io::Result<String> {
    let mut base = sanitize_name(name);
    if !is_path_part_portable(&base) {
        base = format!("_{base}_");
    }
    let mut count = 0u32;
    if let Some((stem, parsed)) = split_copy_counter(&base) {
        count = parsed;
        base = stem.to_owned();
    }
    let room = MAX_FILE_NAME.saturating_sub(suffix.chars().count());
    base = truncate_chars(&base, room).to_owned();
    loop {
        let candidate = if count == 0 {
            base.clone()
        } else {
            let tail = format!(" ({count})");
            let room = MAX_FILE_NAME - tail.len();
            format!("{}{tail}", truncate_chars(&base, room))
        };
        let full = format!("{candidate}{suffix}");
        if !base_dir.join(&full).exists() {
            return Ok(full);
        }
        count += 1;
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn available_names_count_up_past_existing_directories() {
        let dir = crate::test_dir::TestDir::new();
        let root = dir.path();
        assert_eq!(
            find_available_name(root, "My World", "").unwrap(),
            "My World"
        );
        fs::create_dir(root.join("My World")).unwrap();
        assert_eq!(
            find_available_name(root, "My World", "").unwrap(),
            "My World (1)"
        );
        fs::create_dir(root.join("My World (1)")).unwrap();
        assert_eq!(
            find_available_name(root, "My World (1)", "").unwrap(),
            "My World (2)"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn names_are_sanitised_portable_and_bounded() {
        let dir = crate::test_dir::TestDir::new();
        let root = dir.path();
        assert_eq!(find_available_name(root, "a/b:c.d", "").unwrap(), "a_b_c_d");
        assert_eq!(find_available_name(root, "CON", "").unwrap(), "_CON_");
        let long = "x".repeat(400);
        assert_eq!(find_available_name(root, &long, ".zip").unwrap().len(), 255);
    }
}
