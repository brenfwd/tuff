use std::collections::BTreeMap;
use std::io;
use std::path::Path;

pub const DEFAULT_LANGUAGE: &str = "en_us";

/// The `codeofconduct/<language>.txt` files of the instance directory, keyed
/// by lowercased language, with colour codes stripped and lines joined by
/// CRLF.
pub fn read(instance_dir: &Path) -> io::Result<BTreeMap<String, String>> {
    let directory = instance_dir.join("codeofconduct");
    let root = std::fs::canonicalize(&directory).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("code of conduct folder {}: {err}", directory.display()),
        )
    })?;
    let mut texts = BTreeMap::new();
    for entry in std::fs::read_dir(&root)? {
        let path = entry?.path();
        let Some(language) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".txt"))
        else {
            continue;
        };
        if std::fs::canonicalize(&path)?.parent() != Some(root.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("code of conduct file {language}.txt links outside its folder"),
            ));
        }
        let text = std::fs::read_to_string(&path)?;
        let joined = text.lines().collect::<Vec<_>>().join("\r\n");
        texts.insert(language.to_ascii_lowercase(), strip_colors(&joined));
    }
    Ok(texts)
}

/// The text for the client's language, else the default language's, else
/// the first one.
pub fn pick<'a>(texts: &'a [(String, String)], language: &str) -> Option<&'a str> {
    let language = language.to_ascii_lowercase();
    [language.as_str(), DEFAULT_LANGUAGE]
        .iter()
        .find_map(|wanted| texts.iter().find(|(name, _)| name == wanted))
        .or_else(|| texts.first())
        .map(|(_, text)| text.as_str())
}

fn strip_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{00A7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn files_are_keyed_by_language_and_stripped() {
        let dir = tuff_storage::test_dir::TestDir::new();
        let folder = dir.path().join("codeofconduct");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("EN_US.txt"), "be \u{00A7}anice\nplease\n").unwrap();
        std::fs::write(folder.join("de_de.txt"), "seid nett").unwrap();
        std::fs::write(folder.join("notes.md"), "ignored").unwrap();
        let texts: Vec<_> = read(dir.path()).unwrap().into_iter().collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(pick(&texts, "en_US"), Some("be nice\r\nplease"));
        assert_eq!(pick(&texts, "DE_DE"), Some("seid nett"));
        assert_eq!(pick(&texts, "fr_fr"), Some("be nice\r\nplease"));
        assert_eq!(pick(&[], "en_us"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_missing_folder_is_an_error() {
        let dir = tuff_storage::test_dir::TestDir::new();
        assert!(read(dir.path()).is_err());
    }
}
