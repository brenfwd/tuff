use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha1::{Digest, Sha1};
use tuff_client_harness::meta::{Platform, VersionMeta, expand_args};
use tuff_client_harness::{AssetProfile, Source, ensure_client};

const FIXTURE: &str = include_str!("fixtures/version.json");

fn sha(data: &[u8]) -> String {
    Sha1::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn platform(os: &'static str, arch: &'static str) -> Platform {
    Platform { os, arch }
}

fn names(meta: &VersionMeta, p: &Platform) -> Vec<String> {
    meta.libraries_for(p)
        .unwrap()
        .iter()
        .map(|a| a.path.rsplit('/').next().unwrap().to_owned())
        .collect()
}

fn vars() -> std::collections::BTreeMap<String, String> {
    [
        ("auth_player_name", "Bot"),
        ("resolution_width", "960"),
        ("classpath", "CP"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

#[tuff_core::watchdog]
#[test]
fn library_rules_per_platform() {
    let meta = VersionMeta::parse(FIXTURE.as_bytes()).unwrap();
    assert_eq!(
        names(&meta, &platform("osx", "arm64")),
        [
            "common-1.jar",
            "nat-1-natives-macos-arm64.jar",
            "last-1.jar"
        ]
    );
    assert_eq!(
        names(&meta, &platform("osx", "x86_64")),
        ["common-1.jar", "last-1.jar"]
    );
    assert_eq!(
        names(&meta, &platform("linux", "x86_64")),
        ["common-1.jar", "nat-1-natives-linux.jar", "last-1.jar"]
    );
    assert_eq!(
        names(&meta, &platform("windows", "x86_64")),
        [
            "common-1.jar",
            "nat-1-natives-windows-x86.jar",
            "last-1.jar"
        ]
    );
    assert_eq!(
        names(&meta, &platform("windows", "x86")),
        ["common-1.jar", "last-1.jar"],
        "last matching rule wins"
    );
}

#[tuff_core::watchdog]
#[test]
fn args_are_filtered_and_templated() {
    let meta = VersionMeta::parse(FIXTURE.as_bytes()).unwrap();
    let game = expand_args(&meta.arguments.game, &platform("linux", "x86_64"), &vars()).unwrap();
    assert_eq!(game, ["--username", "Bot", "--width", "960"]);
    let mac = expand_args(&meta.arguments.jvm, &platform("osx", "arm64"), &vars()).unwrap();
    assert_eq!(mac, ["-XstartOnFirstThread", "-cp", "CP"]);
    let win = expand_args(&meta.arguments.jvm, &platform("windows", "x86_64"), &vars()).unwrap();
    assert_eq!(win, ["-cp", "CP"], "max version range is not met");
}

#[tuff_core::watchdog]
#[test]
fn unknown_placeholder_and_rule_keys_fail() {
    let meta = VersionMeta::parse(FIXTURE.as_bytes()).unwrap();
    assert!(
        expand_args(
            &meta.arguments.game,
            &platform("linux", "x86_64"),
            &Default::default()
        )
        .is_err()
    );
    let bad = FIXTURE.replace(
        "\"features\": {\"is_demo_user\"",
        "\"surprise\": 1, \"features\": {\"is_demo_user\"",
    );
    assert!(VersionMeta::parse(bad.as_bytes()).is_err());
}

#[tuff_core::watchdog]
#[test]
fn asset_profiles_filter() {
    assert!(AssetProfile::Full.includes("minecraft/sounds/a.ogg"));
    let m = AssetProfile::Minimal;
    assert!(!m.includes("minecraft/sounds/a.ogg"));
    assert!(!m.includes("minecraft/lang/de_de.json"));
    assert!(m.includes("minecraft/sounds.json"));
    assert!(m.includes("minecraft/font/x.png"));
}

struct Mock {
    files: HashMap<String, Vec<u8>>,
    hits: AtomicUsize,
    seen: Mutex<Vec<String>>,
}

impl Source for Mock {
    fn get(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.seen.lock().unwrap().push(url.to_owned());
        self.files
            .get(url)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("offline: {url}"))
    }
}

fn mock() -> Mock {
    let mut files = HashMap::new();
    let mut json = FIXTURE.to_owned();
    let mut put = |placeholder: &str, url: &str, data: &[u8], json: &mut String| {
        *json = json.replace(placeholder, &sha(data));
        files.insert(url.to_owned(), data.to_vec());
    };
    put(
        "SHA_JAR",
        "https://example.test/client.jar",
        b"jar",
        &mut json,
    );
    put(
        "SHA_COMMON",
        "https://example.test/common.jar",
        b"c",
        &mut json,
    );
    put("SHA_MAC", "https://example.test/mac.jar", b"m", &mut json);
    put(
        "SHA_LINUX",
        "https://example.test/linux.jar",
        b"l",
        &mut json,
    );
    put(
        "SHA_WINX86",
        "https://example.test/winx86.jar",
        b"w",
        &mut json,
    );
    put("SHA_LAST", "https://example.test/last.jar", b"z", &mut json);
    let (snd, lang, tex) = (b"snd".as_slice(), b"lang".as_slice(), b"tex".as_slice());
    let index = format!(
        r#"{{"objects":{{"minecraft/sounds/a.ogg":{{"hash":"{s}","size":3}},"minecraft/sounds/b.ogg":{{"hash":"{s}","size":3}},"minecraft/lang/x.json":{{"hash":"{l}","size":4}},"minecraft/font/t.png":{{"hash":"{t}","size":3}}}}}}"#,
        s = sha(snd),
        l = sha(lang),
        t = sha(tex)
    );
    put(
        "SHA_INDEX",
        "https://example.test/index.json",
        index.as_bytes(),
        &mut json,
    );
    for data in [snd, lang, tex] {
        let h = sha(data);
        files.insert(
            format!("https://resources.download.minecraft.net/{}/{h}", &h[..2]),
            data.to_vec(),
        );
    }
    files.insert(
        "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json".into(),
        br#"{"versions":[{"id":"9.9","url":"https://example.test/9.9.json"}]}"#.to_vec(),
    );
    files.insert("https://example.test/9.9.json".into(), json.into_bytes());
    Mock {
        files,
        hits: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tuff-client-fetch-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[tuff_core::watchdog]
#[test]
fn fetch_caches_and_is_offline_afterwards() {
    let root = scratch("full");
    let src = mock();
    let p = platform("linux", "x86_64");
    let files = ensure_client(&src, &root, "9.9", AssetProfile::Full, &p).unwrap();
    assert_eq!(fs::read(&files.client_jar).unwrap(), b"jar");
    let cp: Vec<_> = files
        .classpath
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        cp,
        [
            "common-1.jar",
            "nat-1-natives-linux.jar",
            "last-1.jar",
            "client.jar"
        ]
    );
    let objects = files.assets_dir.join("objects");
    for data in [b"snd".as_slice(), b"lang", b"tex"] {
        let h = sha(data);
        assert!(objects.join(&h[..2]).join(&h).is_file());
    }
    assert!(files.assets_dir.join("indexes/99.json").is_file());
    assert!(files.natives_dir.join("lwjgl").is_dir());
    assert_eq!(
        src.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|u| u.contains("resources.download"))
            .count(),
        3,
        "duplicate hashes download once"
    );

    let before = src.hits.load(Ordering::Relaxed);
    let offline = Mock {
        files: HashMap::new(),
        hits: AtomicUsize::new(0),
        seen: Mutex::new(Vec::new()),
    };
    ensure_client(&offline, &root, "9.9", AssetProfile::Minimal, &p).unwrap();
    assert!(before > 0 && offline.hits.load(Ordering::Relaxed) == 0);
    let _ = fs::remove_dir_all(&root);
}

#[tuff_core::watchdog]
#[test]
fn minimal_profile_skips_sounds_and_does_not_satisfy_full() {
    let root = scratch("minimal");
    let src = mock();
    let p = platform("linux", "x86_64");
    let files = ensure_client(&src, &root, "9.9", AssetProfile::Minimal, &p).unwrap();
    let h = sha(b"snd");
    assert!(
        !files
            .assets_dir
            .join("objects")
            .join(&h[..2])
            .join(&h)
            .exists()
    );
    ensure_client(&src, &root, "9.9", AssetProfile::Full, &p).unwrap();
    assert!(
        files
            .assets_dir
            .join("objects")
            .join(&h[..2])
            .join(&h)
            .is_file()
    );
    let _ = fs::remove_dir_all(&root);
}

#[tuff_core::watchdog]
#[test]
fn corrupt_download_is_rejected() {
    let root = scratch("corrupt");
    let mut src = mock();
    src.files
        .insert("https://example.test/common.jar".into(), b"bad".to_vec());
    let err = ensure_client(
        &src,
        &root,
        "9.9",
        AssetProfile::Full,
        &platform("linux", "x86_64"),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("sha1 mismatch"));
    let _ = fs::remove_dir_all(&root);
}

#[tuff_core::watchdog]
#[test]
#[ignore = "downloads the real client (about 600 MB) from Mojang"]
fn fetch_real_client() {
    let src = tuff_client_harness::HttpSource::new().unwrap();
    let files = ensure_client(
        &src,
        &tuff_client_harness::cache_dir(),
        tuff_vanilla_data::version::NAME,
        AssetProfile::from_env().unwrap(),
        &Platform::current(),
    )
    .unwrap();
    println!("{files:#?}");
}
