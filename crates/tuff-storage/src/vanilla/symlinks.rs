//! Symlink validation of a file-backed world directory against the
//! instance's `allowed_symlinks.txt`, refusing to load a world whose
//! symlinks point anywhere the operator has not allowed.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regex::Regex;

pub const ALLOWED_SYMLINKS_FILE: &str = "allowed_symlinks.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Prefix(String),
    Glob(String),
    Regex(String),
}

impl Entry {
    fn parse(line: &str) -> Result<Option<Self>, String> {
        if line.trim().is_empty() || line.starts_with('#') {
            return Ok(None);
        }
        let Some(rest) = line.strip_prefix('[') else {
            return Ok(Some(Self::Prefix(line.to_owned())));
        };
        let Some(split) = rest.find(']') else {
            return Err(format!("Unterminated type in line '{line}'"));
        };
        let contents = rest[split + 1..].to_owned();
        match &rest[..split] {
            "glob" => Ok(Some(Self::Glob(contents))),
            "regex" => Ok(Some(Self::Regex(contents))),
            "prefix" => Ok(Some(Self::Prefix(contents))),
            _ => Err(format!("Unsupported definition type in line '{line}'")),
        }
    }
}

enum Matcher {
    Prefix(String),
    Pattern(Regex),
}

/// The parsed allow list. An empty or unreadable list allows nothing.
pub struct SymlinkAllowList {
    matchers: Vec<Matcher>,
}

impl SymlinkAllowList {
    pub fn deny_all() -> Self {
        Self {
            matchers: Vec::new(),
        }
    }

    /// Parses the file's text. Any malformed line rejects the whole list;
    /// a pattern that does not compile makes the list match nothing.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut entries = Vec::new();
        for line in text.lines() {
            if let Some(entry) = Entry::parse(line)? {
                entries.push(entry);
            }
        }
        let mut matchers = Vec::with_capacity(entries.len());
        for entry in entries {
            let compiled = match entry {
                Entry::Prefix(prefix) => Ok(Matcher::Prefix(prefix)),
                Entry::Glob(glob) => glob_to_regex(&glob)
                    .and_then(|re| Regex::new(&re).map_err(|e| e.to_string()))
                    .map(Matcher::Pattern),
                Entry::Regex(re) => Regex::new(&format!("^(?:{re})$"))
                    .map(Matcher::Pattern)
                    .map_err(|e| e.to_string()),
            };
            match compiled {
                Ok(matcher) => matchers.push(matcher),
                Err(err) => {
                    log::error!("Failed to compile file pattern list: {err}");
                    return Ok(Self::deny_all());
                }
            }
        }
        Ok(Self { matchers })
    }

    /// Reads `path`; a missing file allows nothing, and a malformed one is
    /// logged and allows nothing.
    pub fn load(path: &Path) -> Self {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Self::deny_all(),
            Err(err) => {
                log::error!(
                    "Failed to parse {ALLOWED_SYMLINKS_FILE}, disallowing all symbolic links: {err}"
                );
                return Self::deny_all();
            }
        };
        Self::parse(&text).unwrap_or_else(|err| {
            log::error!(
                "Failed to parse {ALLOWED_SYMLINKS_FILE}, disallowing all symbolic links: {err}"
            );
            Self::deny_all()
        })
    }

    pub fn matches(&self, target: &Path) -> bool {
        let target = target.to_string_lossy();
        self.matchers.iter().any(|m| match m {
            Matcher::Prefix(prefix) => target.starts_with(prefix.as_str()),
            Matcher::Pattern(re) => re.is_match(&target),
        })
    }
}

/// Converts a filesystem glob (`*`, `**`, `?`, `[...]`, `{a,b}`, `\`
/// escapes; separators are `/`) into an anchored regex.
fn glob_to_regex(glob: &str) -> Result<String, String> {
    let mut out = String::from("^");
    let mut chars = glob.chars().peekable();
    let mut in_group = false;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(next) = chars.next() else {
                    return Err("No character to escape".into());
                };
                push_literal(&mut out, next);
            }
            '/' => out.push('/'),
            '[' => {
                out.push_str("[[^/]&&[");
                if matches!(chars.peek(), Some('^')) {
                    chars.next();
                    out.push_str("\\^");
                } else if matches!(chars.peek(), Some('!')) {
                    chars.next();
                    out.push('^');
                }
                if matches!(chars.peek(), Some('-')) {
                    chars.next();
                    out.push('-');
                }
                let mut closed = false;
                let mut last: Option<char> = None;
                while let Some(c) = chars.next() {
                    match c {
                        ']' => {
                            closed = true;
                            break;
                        }
                        '/' => return Err("Explicit 'name separator' in class".into()),
                        '\\' | '[' | '&' => {
                            out.push('\\');
                            out.push(c);
                        }
                        '-' => {
                            if last.is_none() {
                                return Err("Invalid range".into());
                            }
                            if matches!(chars.peek(), Some(']')) {
                                out.push_str("\\-");
                            } else {
                                out.push('-');
                            }
                            last = None;
                            continue;
                        }
                        _ => out.push(c),
                    }
                    last = Some(c);
                }
                if !closed {
                    return Err("Missing ']".into());
                }
                out.push_str("]]");
            }
            '{' => {
                if in_group {
                    return Err("Cannot nest groups".into());
                }
                out.push_str("(?:(?:");
                in_group = true;
            }
            '}' if in_group => {
                out.push_str("))");
                in_group = false;
            }
            ',' if in_group => out.push_str(")|(?:"),
            '*' => {
                if matches!(chars.peek(), Some('*')) {
                    chars.next();
                    out.push_str(".*");
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            _ => push_literal(&mut out, c),
        }
    }
    if in_group {
        return Err("Missing '}".into());
    }
    out.push('$');
    Ok(out)
}

fn push_literal(out: &mut String, c: char) {
    if regex_syntax_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

fn regex_syntax_meta(c: char) -> bool {
    matches!(
        c,
        '\\' | '.'
            | '+'
            | '*'
            | '?'
            | '('
            | ')'
            | '|'
            | '['
            | ']'
            | '{'
            | '}'
            | '^'
            | '$'
            | '#'
            | '&'
            | '-'
            | '~'
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForbiddenSymlink {
    pub link: PathBuf,
    pub target: PathBuf,
}

/// The world directory holds symlinks the allow list does not cover.
#[derive(Debug)]
pub struct ForbiddenSymlinks {
    pub directory: PathBuf,
    pub links: Vec<ForbiddenSymlink>,
}

impl fmt::Display for ForbiddenSymlinks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to validate '{}'. Found forbidden symlinks: ",
            self.directory.display()
        )?;
        for (i, link) in self.links.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}->{}", link.link.display(), link.target.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for ForbiddenSymlinks {}

impl SymlinkAllowList {
    /// Walks `directory` without following links and lists every symlink
    /// whose target the list does not allow. A symlinked `directory` itself
    /// is followed. A missing directory has no issues.
    pub fn validate_directory(&self, directory: &Path) -> io::Result<Vec<ForbiddenSymlink>> {
        let meta = match fs::symlink_metadata(directory) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        if meta.is_file() {
            return Err(io::Error::other(format!(
                "Path {} is not a directory",
                directory.display()
            )));
        }
        let mut root = directory.to_path_buf();
        if meta.file_type().is_symlink() {
            let target = fs::read_link(directory)?;
            root = match directory.parent() {
                Some(parent) if target.is_relative() => parent.join(target),
                _ => target,
            };
        }
        let mut issues = Vec::new();
        self.walk(&root, &mut issues)?;
        Ok(issues)
    }

    fn walk(&self, dir: &Path, issues: &mut Vec<ForbiddenSymlink>) -> io::Result<()> {
        let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                let target = fs::read_link(&path)?;
                if !self.matches(&target) {
                    issues.push(ForbiddenSymlink { link: path, target });
                }
            } else if file_type.is_dir() {
                self.walk(&path, issues)?;
            }
        }
        Ok(())
    }
}

/// Validates a world directory against `allowed_symlinks.txt` in its parent
/// (the instance directory), failing with the forbidden links.
pub fn validate_world_dir(world: &Path) -> io::Result<()> {
    let parent = match world.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let list = SymlinkAllowList::load(&parent.join(ALLOWED_SYMLINKS_FILE));
    let links = list.validate_directory(world)?;
    if links.is_empty() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        ForbiddenSymlinks {
            directory: world.to_path_buf(),
            links,
        },
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;
    use std::os::unix::fs::symlink;

    fn list(text: &str) -> SymlinkAllowList {
        SymlinkAllowList::parse(text).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn entries_parse_like_the_config_format() {
        let l = list(
            "# comment\n\n/srv/shared\n[prefix]/opt/\n[glob]/data/*/maps\n[regex]/mnt/disk[0-9]+\n",
        );
        assert!(l.matches(Path::new("/srv/shared/x")));
        assert!(l.matches(Path::new("/opt/a")));
        assert!(l.matches(Path::new("/data/one/maps")));
        assert!(!l.matches(Path::new("/data/one/two/maps")));
        assert!(l.matches(Path::new("/mnt/disk12")));
        assert!(!l.matches(Path::new("/mnt/disk12/x")));
        assert!(!l.matches(Path::new("/etc/passwd")));
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_lines_reject_the_list() {
        assert_eq!(
            SymlinkAllowList::parse("[glob/x").err().unwrap(),
            "Unterminated type in line '[glob/x'"
        );
        assert_eq!(
            SymlinkAllowList::parse("[nope]x").err().unwrap(),
            "Unsupported definition type in line '[nope]x'"
        );
        assert!(!list("[regex](").matches(Path::new("(")));
    }

    #[tuff_core::watchdog]
    #[test]
    fn globs_follow_filesystem_glob_rules() {
        let l = list("[glob]/a/**\n[glob]/b/{x,y}?\n[glob]/c/[!q]");
        assert!(l.matches(Path::new("/a/b/c")));
        assert!(l.matches(Path::new("/b/x1")));
        assert!(l.matches(Path::new("/b/yz")));
        assert!(!l.matches(Path::new("/b/zz")));
        assert!(l.matches(Path::new("/c/p")));
        assert!(!l.matches(Path::new("/c/q")));
        assert!(!l.matches(Path::new("/c/pp")));
    }

    #[tuff_core::watchdog]
    #[test]
    fn world_dir_with_forbidden_symlink_fails_to_validate() {
        let dir = TestDir::new();
        let world = dir.path().join("world");
        fs::create_dir_all(world.join("region")).unwrap();
        symlink("/tmp/elsewhere", world.join("region/r.0.0.mca")).unwrap();
        let err = validate_world_dir(&world).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "Failed to validate '{}'. Found forbidden symlinks: {}->/tmp/elsewhere",
                world.display(),
                world.join("region/r.0.0.mca").display()
            )
        );
        fs::write(dir.path().join(ALLOWED_SYMLINKS_FILE), "/tmp/\n").unwrap();
        validate_world_dir(&world).unwrap();
    }

    #[tuff_core::watchdog]
    #[test]
    fn top_level_world_symlink_is_followed() {
        let dir = TestDir::new();
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, dir.path().join("world")).unwrap();
        validate_world_dir(&dir.path().join("world")).unwrap();
        symlink("/nowhere", real.join("bad")).unwrap();
        assert!(validate_world_dir(&dir.path().join("world")).is_err());
        validate_world_dir(&dir.path().join("missing")).unwrap();
    }
}
