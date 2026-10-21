//! `tuff plugin new <id>`: writes a ready-to-build plugin project from the
//! SDK's templates (`crates/tuff-plugin-sdk/templates`). Rendered with id
//! `hello` the templates are exactly `examples/rust-hello` and
//! `examples/cpp-hello`, which CI builds and tests, so a new project starts
//! from code known to work.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const ACTION: &str = "new";

/// The longest id that is also a valid command name.
const MAX_ID: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Cpp,
}

/// One file of a new project: its path inside the project and contents.
#[derive(Debug, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub contents: String,
    pub executable: bool,
}

const RUST_LIB: &str = include_str!("../crates/tuff-plugin-sdk/templates/rust/lib.rs");
const RUST_CARGO: &str = include_str!("../crates/tuff-plugin-sdk/templates/rust/Cargo.toml");
const RUST_CONFIG: &str = include_str!("../crates/tuff-plugin-sdk/templates/rust/config.toml");
const RUST_IGNORE: &str = include_str!("../crates/tuff-plugin-sdk/templates/rust/gitignore");
const CPP_PLUGIN: &str = include_str!("../crates/tuff-plugin-sdk/templates/cpp/plugin.cpp");
const CPP_BUILD: &str = include_str!("../crates/tuff-plugin-sdk/templates/cpp/build.sh");
const CPP_IGNORE: &str = include_str!("../crates/tuff-plugin-sdk/templates/cpp/gitignore");

pub fn usage() -> String {
    let bin = env!("CARGO_PKG_NAME");
    format!(
        "usage: {bin} plugin {ACTION} <id> [--lang rust|cpp] [--dir <dir>] [--sdk <dir>]

Writes a plugin project for id <id> (1 to {MAX_ID} of a-z 0-9 - _) into
<dir> (default ./<id>), which must not exist or be empty. --sdk names the
SDK (crates/tuff-plugin-sdk of a tuff checkout); by default the one this
binary was built from.
"
    )
}

/// `id` as a type name: `my-plugin` is `MyPlugin`.
fn type_name(id: &str) -> String {
    let mut out = String::new();
    for word in id.split(['-', '_']).filter(|w| !w.is_empty()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if !out.starts_with(|c: char| c.is_ascii_alphabetic()) {
        out.insert(0, 'P');
    }
    out
}

fn valid(id: &str) -> bool {
    tuff_plugin_wasm::manifest::valid_id(id) && id.len() <= MAX_ID
}

/// The files of a new `lang` project for plugin `id`, with the SDK at
/// `sdk` (as the project will spell the path).
pub fn render(lang: Lang, id: &str, sdk: &str) -> Result<Vec<File>> {
    if !valid(id) {
        bail!("plugin id {id:?} must be 1 to {MAX_ID} of a-z 0-9 - _");
    }
    let fill = |template: &str| {
        template
            .replace("__ID__", id)
            .replace("__NAME__", &type_name(id))
            .replace("__SDK__", sdk)
    };
    let file = |path: &str, template: &str| File {
        path: PathBuf::from(path),
        contents: fill(template),
        executable: false,
    };
    Ok(match lang {
        Lang::Rust => vec![
            File {
                contents: RUST_CARGO
                    .replace("__ID__", id)
                    .replace("__SDK__", &sdk.replace('\\', "/").replace('"', "\\\"")),
                ..file("Cargo.toml", "")
            },
            file(".cargo/config.toml", RUST_CONFIG),
            file(".gitignore", RUST_IGNORE),
            file("src/lib.rs", RUST_LIB),
        ],
        Lang::Cpp => vec![
            file(&format!("{id}.cpp"), CPP_PLUGIN),
            File {
                executable: true,
                ..file("build.sh", CPP_BUILD)
            },
            file(".gitignore", CPP_IGNORE),
        ],
    })
}

fn default_sdk() -> Option<PathBuf> {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/tuff-plugin-sdk");
    sdk.join("Cargo.toml").is_file().then_some(sdk)
}

fn write(dir: &Path, files: &[File]) -> Result<()> {
    if dir.exists() && dir.read_dir()?.next().is_some() {
        bail!("{} exists and is not empty", dir.display());
    }
    for file in files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.contents)
            .with_context(|| format!("cannot write {}", path.display()))?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// Runs the arguments after `plugin new` and returns what to print.
pub fn run(args: &[String]) -> Result<String> {
    let (mut id, mut lang, mut dir, mut sdk) = (None, Lang::Rust, None, None);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || {
            iter.next()
                .cloned()
                .with_context(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "--lang" => {
                lang = match value()?.as_str() {
                    "rust" => Lang::Rust,
                    "cpp" | "c++" => Lang::Cpp,
                    other => bail!("unknown language {other:?}; rust or cpp"),
                }
            }
            "--dir" => dir = Some(PathBuf::from(value()?)),
            "--sdk" => sdk = Some(PathBuf::from(value()?)),
            "help" | "--help" | "-h" => return Ok(usage()),
            other if other.starts_with("--") => bail!("unknown option {other:?}\n{}", usage()),
            other if id.is_none() => id = Some(other.to_owned()),
            other => bail!("unexpected argument {other:?}\n{}", usage()),
        }
    }
    let Some(id) = id else {
        return Ok(usage());
    };
    let sdk = match sdk.or_else(default_sdk) {
        Some(sdk) => std::path::absolute(sdk)?,
        None => bail!("cannot find the plugin SDK; name it with --sdk"),
    };
    let dir = dir.unwrap_or_else(|| PathBuf::from(&id));
    let files = render(lang, &id, &sdk.display().to_string())?;
    write(&dir, &files)?;
    let build = match lang {
        Lang::Rust => "cargo build --release",
        Lang::Cpp => "./build.sh",
    };
    Ok(format!(
        "wrote {} in {}\nbuild it with `{build}` there, copy the .wasm into the server's plugins \
         directory and run /tuff plugin load <file>\n",
        files.len(),
        dir.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example(path: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_templates_render_to_the_tested_examples() {
        let sdk = "../../crates/tuff-plugin-sdk";
        for (lang, dir) in [(Lang::Rust, "rust-hello"), (Lang::Cpp, "cpp-hello")] {
            for file in render(lang, "hello", sdk).unwrap() {
                let path = format!("examples/{dir}/{}", file.path.display());
                assert_eq!(
                    file.contents,
                    example(&path),
                    "{path} differs from its template"
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn ids_become_type_names_and_bad_ids_are_refused() {
        assert_eq!(type_name("hello"), "Hello");
        assert_eq!(type_name("my-cool_plugin"), "MyCoolPlugin");
        assert_eq!(type_name("2fa"), "P2fa");
        assert!(render(Lang::Rust, "Bad", "sdk").is_err());
        assert!(render(Lang::Rust, &"a".repeat(33), "sdk").is_err());
        let files = render(Lang::Cpp, "my-plugin", "/sdk").unwrap();
        assert_eq!(files[0].path, PathBuf::from("my-plugin.cpp"));
        assert!(files[0].contents.contains("TUFF_PLUGIN(MyPlugin"));
        assert!(files[1].contents.contains("/sdk/c/build.sh"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_project_is_written_only_into_an_empty_directory() {
        let dir = std::env::temp_dir().join(format!("tuff-plugin-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let args: Vec<String> = ["greeter", "--dir", dir.to_str().unwrap(), "--sdk", "/sdk"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        run(&args).unwrap();
        let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"greeter\""));
        assert!(cargo.contains("tuff-plugin-sdk = { path = \"/sdk\" }"));
        assert!(
            std::fs::read_to_string(dir.join("src/lib.rs"))
                .unwrap()
                .contains("struct Greeter;")
        );
        assert!(run(&args).unwrap_err().to_string().contains("not empty"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
