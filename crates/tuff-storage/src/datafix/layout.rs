//! The 26.1 world layout change for vanilla world directories: every
//! dimension under `dimensions/<namespace>/<name>/`, player files under
//! `players/`, namespaced saved data, and `level.dat` split into saved data
//! files. Runs once, before the world is read, while the world is locked.
//!
//! First the whole world is copied to a sibling `<name> OUTDATED`
//! directory (never over an existing one), then only moves and new files:
//! nothing is deleted except the directories the moves leave empty and the
//! obsolete files the game itself drops. `level.dat` is rewritten last (its
//! previous contents kept as `level.dat_old`), so an interrupted upgrade
//! runs again from the start, finds the moves already made, and keeps the
//! backup the marker file names instead of copying the half-moved world.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tuff_vanilla_data::version::DATA_VERSION;

use super::level::{self, SPLIT_VERSION};
use super::{DataFixError, Fixer, MIN_DATA_VERSION, get_bool, get_number};
use crate::files::{read_nbt_file, safe_replace, write_nbt_file};
use tuff_nbt::{Compound, Tag};

/// The version that re-enabled spectator chunk generation in hardcore
/// worlds first played in singleplayer.
const SPECTATOR_FIX_VERSION: i32 = 4899;
const DIMENSIONS: [(&str, &str); 3] = [
    ("", "overworld"),
    ("DIM-1", "the_nether"),
    ("DIM1", "the_end"),
];
const LEGACY_STRUCTURES: [&str; 9] = [
    "Monument",
    "Stronghold",
    "Mineshaft",
    "Temple",
    "Mansion",
    "Village",
    "Fortress",
    "EndCity",
    "villages",
];
const BACKUP_SUFFIX: &str = " OUTDATED";
const BACKUP_MARKER: &str = "upgrade_backup.txt";
const SESSION_LOCK: &str = "session.lock";
const MAX_COPY_DEPTH: usize = 64;

/// Upgrades the world at `root` if its `level.dat` predates the current
/// layout. Returns whether anything was done.
pub fn upgrade_world(root: &Path) -> io::Result<bool> {
    let Some(level) = read_nbt_file(&root.join("level.dat"))? else {
        return Ok(false);
    };
    let Some(data) = level.get_compound("Data") else {
        return Ok(false);
    };
    let Some(version) = get_number(data, "DataVersion").map(|v| v as i32) else {
        return Ok(false);
    };
    if version >= SPECTATOR_FIX_VERSION {
        return Ok(false);
    }
    if version < MIN_DATA_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            DataFixError::TooOld(version),
        ));
    }
    let mut data = data.clone();
    if version < SPLIT_VERSION {
        level::prepare(&mut data, Fixer::new(version))
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        let backup = back_up(root)?;
        log::info!(
            "upgrading the world in {} from data version {version}, backed up to {}",
            root.display(),
            backup.display()
        );
        move_files(root)?;
        split_level(root, &mut data)?;
    }
    reenable_spectator_generation(root, &data)?;
    data.insert("DataVersion", DATA_VERSION);
    let mut level = level;
    level.insert("Data", data);
    write_nbt_file(&root.join("level.dat"), level)?;
    remove_file(&root.join(BACKUP_MARKER))?;
    Ok(true)
}

/// Copies the world at `root` to a new sibling directory before anything
/// is moved, or returns the one an interrupted upgrade already made. The
/// copy is staged under a hidden name and renamed into place only once
/// complete, and the marker naming it is written before the first move.
pub(super) fn back_up(root: &Path) -> io::Result<PathBuf> {
    let root = root.canonicalize()?;
    let (Some(parent), Some(name)) = (root.parent(), root.file_name()) else {
        return Err(io::Error::other(format!(
            "{} has no parent directory to back up into",
            root.display()
        )));
    };
    let name = name.to_string_lossy();
    let marker = root.join(BACKUP_MARKER);
    match fs::read_to_string(&marker) {
        Ok(recorded) if valid_backup_name(recorded.trim()) => {
            let backup = parent.join(recorded.trim());
            if !backup.is_dir() {
                log::warn!(
                    "the backup {} of an interrupted world upgrade is missing",
                    backup.display()
                );
            }
            return Ok(backup);
        }
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let staging = parent.join(format!(".{name}{BACKUP_SUFFIX}.partial"));
    if fs::symlink_metadata(&staging).is_ok() {
        fs::remove_dir_all(&staging)?;
    }
    copy_dir(&root, &staging, true, 0)?;
    let backup = available_dir(parent, &format!("{name}{BACKUP_SUFFIX}"));
    fs::rename(&staging, &backup)?;
    let file_name = backup.file_name().unwrap_or_default().to_string_lossy();
    safe_replace(&marker, file_name.as_bytes())?;
    Ok(backup)
}

fn valid_backup_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// `base`, or `base (1)`, `base (2)`... whichever does not exist yet.
fn available_dir(parent: &Path, base: &str) -> PathBuf {
    let mut count = 0u32;
    loop {
        let name = if count == 0 {
            base.to_owned()
        } else {
            format!("{base} ({count})")
        };
        let path = parent.join(name);
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
        count += 1;
    }
}

/// Copies a directory tree, following symlinks so the copy holds real
/// data; the world's own lock file is left out.
fn copy_dir(from: &Path, to: &Path, top: bool, depth: usize) -> io::Result<()> {
    if depth > MAX_COPY_DEPTH {
        return Err(io::Error::other(format!(
            "{} is nested too deeply to back up",
            from.display()
        )));
    }
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && (name == SESSION_LOCK || name == BACKUP_MARKER) {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        if fs::metadata(&source)?.is_dir() {
            copy_dir(&source, &target, false, depth + 1)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

fn move_files(root: &Path) -> io::Result<()> {
    for dimension in custom_dimension_dirs(root)? {
        let data = dimension.join("data");
        for (from, to) in [
            ("chunks.dat", "minecraft/chunk_tickets.dat"),
            ("raids.dat", "minecraft/raids.dat"),
            ("world_border.dat", "minecraft/world_border.dat"),
        ] {
            move_path(&data.join(from), &data.join(to))?;
        }
    }
    for (old, name) in DIMENSIONS {
        let from = root.join(old).join("data");
        let to = new_dimension(root, name).join("data").join("minecraft");
        for (file, target) in [
            ("chunks.dat", "chunk_tickets.dat"),
            ("raids.dat", "raids.dat"),
            ("raids_end.dat", "raids.dat"),
            ("world_border.dat", "world_border.dat"),
        ] {
            move_path(&from.join(file), &to.join(target))?;
        }
    }
    for old in ["DIM1", "DIM-1"] {
        remove_empty_dir(&root.join(old).join("data"))?;
    }
    let data = root.join("data");
    for (from, to) in [
        ("scoreboard.dat", "minecraft/scoreboard.dat"),
        ("stopwatches.dat", "minecraft/stopwatches.dat"),
        ("idcounts.dat", "minecraft/maps/last_id.dat"),
        ("random_sequences.dat", "minecraft/random_sequences.dat"),
    ] {
        move_path(&data.join(from), &data.join(to))?;
    }
    for name in file_names(&data)? {
        if let Some(namespace) = name
            .strip_prefix("command_storage_")
            .and_then(|n| n.strip_suffix(".dat"))
            .filter(|n| valid_namespace(n))
        {
            move_path(
                &data.join(&name),
                &data.join(namespace).join("command_storage.dat"),
            )?;
        } else if let Some(id) = name
            .strip_prefix("map_")
            .and_then(|n| n.strip_suffix(".dat"))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            move_path(
                &data.join(&name),
                &data
                    .join("minecraft")
                    .join("maps")
                    .join(format!("{id}.dat")),
            )?;
        }
    }
    for (old, name) in DIMENSIONS {
        for kind in ["region", "entities", "poi"] {
            move_path(
                &root.join(old).join(kind),
                &new_dimension(root, name).join(kind),
            )?;
        }
    }
    for old in ["DIM-1", "DIM1"] {
        remove_empty_dir(&root.join(old))?;
    }
    for (from, to) in [
        ("advancements", "players/advancements"),
        ("playerdata", "players/data"),
        ("stats", "players/stats"),
        ("resources.zip", "resourcepacks/resources.zip"),
    ] {
        move_path(&root.join(from), &root.join(to))?;
    }
    for name in ["villages_end.dat", "villages_nether.dat"] {
        remove_file(&data.join(name))?;
    }
    for structure in LEGACY_STRUCTURES {
        remove_file(&data.join(format!("{structure}.dat")))?;
        remove_file(&data.join(format!("{structure}_index.dat")))?;
    }
    for dir in ["advancements", "functions"] {
        remove_empty_dir(&data.join(dir))?;
    }
    if let Ok(entries) = fs::read_dir(root.join("generated")) {
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let namespace = entry.path();
                move_path(&namespace.join("structures"), &namespace.join("structure"))?;
            }
        }
    }
    Ok(())
}

fn new_dimension(root: &Path, name: &str) -> PathBuf {
    root.join("dimensions").join("minecraft").join(name)
}

fn valid_namespace(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// Dimension directories that already sat under `dimensions/`.
fn custom_dimension_dirs(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    let Ok(namespaces) = fs::read_dir(root.join("dimensions")) else {
        return Ok(dirs);
    };
    for namespace in namespaces {
        let namespace = namespace?;
        if !namespace.file_type()?.is_dir() {
            continue;
        }
        for name in fs::read_dir(namespace.path())? {
            let name = name?;
            if name.file_type()?.is_dir() {
                dirs.push(name.path());
            }
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn file_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(names);
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Some(name) = entry.file_name().to_str()
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Moves a file or directory; a directory moved onto an existing one is
/// merged entry by entry. Nothing that already exists is overwritten.
fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    let Ok(source) = fs::symlink_metadata(from) else {
        return Ok(());
    };
    match fs::symlink_metadata(to) {
        Err(_) => {
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(from, to)
        }
        Ok(target) if source.is_dir() && target.is_dir() => {
            for entry in fs::read_dir(from)? {
                let entry = entry?;
                move_path(&entry.path(), &to.join(entry.file_name()))?;
            }
            remove_empty_dir(from)
        }
        Ok(_) => {
            log::warn!(
                "not moving {} during the world upgrade: {} already exists",
                from.display(),
                to.display()
            );
            Ok(())
        }
    }
}

fn remove_empty_dir(dir: &Path) -> io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let names: Vec<_> = entries
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    match names.as_slice() {
        [] => fs::remove_dir(dir),
        [only] if only == ".DS_Store" => {
            remove_file(&dir.join(only))?;
            fs::remove_dir(dir)
        }
        _ => Ok(()),
    }
}

fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

fn saved(data: Tag, version: i32) -> Compound {
    Compound::new()
        .with("data", data)
        .with("DataVersion", version)
}

fn write_saved(path: PathBuf, data: Tag) -> io::Result<()> {
    write_nbt_file(&path, saved(data, SPLIT_VERSION))
}

fn data_file(root: &Path, name: &str) -> PathBuf {
    root.join("data")
        .join("minecraft")
        .join(format!("{name}.dat"))
}

fn dimension_file(root: &Path, dimension: &str, name: &str) -> PathBuf {
    new_dimension(root, dimension)
        .join("data")
        .join("minecraft")
        .join(format!("{name}.dat"))
}

/// Moves what 26.1 keeps in saved data files out of the level's `Data`.
fn split_level(root: &Path, data: &mut Compound) -> io::Result<()> {
    if let Some(fight) = data.remove("dragon_fight") {
        write_saved(dimension_file(root, "the_end", "ender_dragon_fight"), fight)?;
    }
    if let Some(Tag::Compound(mut player)) = data.remove("Player") {
        let uuid = match player.get("UUID") {
            Some(Tag::IntArray(uuid)) if uuid.len() == 4 => Tag::IntArray(uuid.clone()),
            _ => {
                player.insert("DataVersion", DATA_VERSION);
                write_nbt_file(
                    &root
                        .join("players")
                        .join("data")
                        .join("00000000-0000-0000-0000-000000000000.dat"),
                    player,
                )?;
                Tag::IntArray(vec![0; 4])
            }
        };
        data.insert("singleplayer_uuid", uuid);
    }
    for (key, file) in [
        ("wandering_trader_migration_data", "wandering_trader"),
        ("CustomBossEvents", "custom_boss_events"),
        ("weather_data", "weather"),
        ("scheduled_events", "scheduled_events"),
    ] {
        if let Some(value) = data.remove(key) {
            write_saved(data_file(root, file), value)?;
        }
    }
    if let Some(Tag::Compound(border)) = data.remove("world_border") {
        for (dimension, divider) in [("overworld", 1.0), ("the_nether", 8.0), ("the_end", 1.0)] {
            let mut scaled = border.clone();
            for key in ["center_x", "center_z"] {
                let value = get_number(&border, key).unwrap_or(0.0) / divider;
                scaled.insert(key, value);
            }
            write_saved(
                dimension_file(root, dimension, "world_border"),
                Tag::Compound(scaled),
            )?;
        }
    }
    for (key, file) in [
        ("game_rules", "game_rules"),
        ("world_gen_settings", "world_gen_settings"),
        ("world_clocks", "world_clocks"),
    ] {
        if let Some(value) = data.remove(key) {
            write_saved(data_file(root, file), value)?;
        }
    }
    Ok(())
}

fn reenable_spectator_generation(root: &Path, data: &Compound) -> io::Result<()> {
    if !data.contains_key("singleplayer_uuid") {
        return Ok(());
    }
    let hardcore = data
        .get_compound("difficulty_settings")
        .and_then(|settings| get_bool(settings, "hardcore"))
        .unwrap_or(false);
    if !hardcore {
        return Ok(());
    }
    let path = data_file(root, "game_rules");
    let Some(mut rules) = read_nbt_file(&path)? else {
        return Ok(());
    };
    let version = get_number(&rules, "DataVersion").map_or(0, |v| v as i32);
    if let Some(inner) = rules.get_compound_mut("data") {
        inner.insert("minecraft:spectators_generate_chunks", true);
    } else {
        rules.insert(
            "data",
            Compound::new().with("minecraft:spectators_generate_chunks", true),
        );
    }
    rules.insert("DataVersion", version.max(SPLIT_VERSION));
    write_nbt_file(&path, rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[tuff_core::watchdog]
    #[test]
    fn a_directory_holding_only_ds_store_is_removed() {
        let dir = TestDir::new();
        let target = dir.path().join("DIM1");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(".DS_Store"), b"x").unwrap();
        remove_empty_dir(&target).unwrap();
        assert!(!target.exists());

        fs::create_dir_all(&target).unwrap();
        fs::write(target.join(".DS_Store"), b"x").unwrap();
        fs::write(target.join("other"), b"x").unwrap();
        remove_empty_dir(&target).unwrap();
        assert!(target.exists());
    }
}
