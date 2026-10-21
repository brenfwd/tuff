use tuff::{backup_cli, console, datapack_cli, features, plugin_cli, replay_cli, trace_cli};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tuff_core::Server;
use tuff_core::ServerHandle;
use tuff_core::logging::{self, HubOptions, LogHub};
use tuff_core::storage::BackendOptions;
use tuff_core::storage::convert::{
    ConvertEndpoint, convert_offline, convert_offline_rewriting, force_upgrade_in_place,
};
use tuff_core::world::DEFAULT_LEVEL_NAME;
use tuff_core::world::tick_barrier::TickLockstep;
use tuff_item::text::TextComponent;
use tuff_storage::sqlite;
use tuff_storage::sqlite::Locking;
use tuff_storage::vanilla;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const OFFLINE_FLAG: &str = "--offline";
const WORLD_BACKEND_FLAG: &str = "--world_backend";
const INSTANCE_DIR_FLAG: &str = "--instance_dir";
const SERVER_FILE_FLAG: &str = "--server_file";
const SQLITE_LOCKING_FLAG: &str = "--sqlite_locking";
const CONVERT_TO_FLAG: &str = "--convert_to";
const CONVERT_OUTPUT_FLAG: &str = "--convert_output";
const FORCE_UPGRADE_FLAG: &str = "--force_upgrade";
const ERASE_CACHE_FLAG: &str = "--erase_cache";
const TICK_LOCKSTEP_FLAG: &str = "--tick_lockstep";
const SEED_FLAG: &str = "--seed";
const CONFIG_FLAG: &str = "--config";
const DEFINE_FLAG: &str = "-D";

fn parse_define(pair: &str) -> Result<(String, String)> {
    match pair.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
        _ => bail!(
            "{DEFINE_FLAG}<key>=<value> needs a key and a value\n{}",
            usage()
        ),
    }
}
const NO_SERVE_FLAG: &str = "--no-serve";
const NO_SAVE_FLAG: &str = "--no-save";
const PID_FILE_FLAG: &str = "--pid_file";
const EXECUTE_FLAG: &str = "--execute";
const WORLD_BACKENDS: [&str; 2] = [vanilla::NAME, sqlite::NAME];

#[derive(Debug, PartialEq)]
struct Args {
    offline: bool,
    world_backend: &'static str,
    instance_dir: PathBuf,
    /// Overrides the server file's default path (`sqlite` only).
    server_file: Option<PathBuf>,
    sqlite_locking: Locking,
    tick_lockstep: TickLockstep,
    /// Seed for a newly created world; ignored for an existing one.
    seed: Option<i64>,
    /// When set, convert the world named by the other flags into this
    /// backend instead of starting the server. `--convert_output` names
    /// where it goes.
    convert_to: Option<&'static str>,
    convert_output: Option<PathBuf>,
    /// Rewrite every chunk of the world through the current codec: into
    /// `--convert_output` (same backend unless `--convert_to` names one), or
    /// in place before the server starts when no output is given.
    force_upgrade: bool,
    /// With `--force_upgrade`, drop cached lighting and heightmaps.
    erase_cache: bool,
    /// `--config <key> <value>` pairs in the order given; a later pair for
    /// the same key wins.
    config: Vec<(String, String)>,
    /// Start everything but bind no listening sockets.
    no_serve: bool,
    /// Read the world but write nothing to disk.
    no_save: bool,
    /// `--pid_file <path>`: where to write the process id at startup.
    pid_file: Option<PathBuf>,
    /// `--execute <file>` in the order given.
    execute: Vec<String>,
    /// Built-in features switched on by `tuff.features.<id>.enabled`.
    features: Vec<String>,
}

impl Args {
    /// The world directory for `vanilla`; for `sqlite` the server file,
    /// which holds the whole server. Both are named after the level inside
    /// the instance directory unless `--server_file` says otherwise.
    /// `None` leaves the vanilla layout's directory to `level-name`.
    fn configured_world_path(&self) -> Option<PathBuf> {
        (self.world_backend == sqlite::NAME).then(|| self.world_path())
    }

    fn world_path(&self) -> PathBuf {
        if self.world_backend != sqlite::NAME {
            return self.instance_dir.join(DEFAULT_LEVEL_NAME);
        }
        if let Some(file) = &self.server_file {
            return file.clone();
        }
        self.instance_dir
            .join(format!("{DEFAULT_LEVEL_NAME}.{}", sqlite::EXTENSION))
    }

    /// The source endpoint for `--convert_to`: `vanilla` takes the instance
    /// directory itself (world at `<instance_dir>/world`, config alongside
    /// it, matching `ConvertEndpoint`); `sqlite` takes the resolved server
    /// file, `--server_file` override included.
    fn convert_source(&self) -> ConvertEndpoint {
        ConvertEndpoint {
            backend: self.world_backend,
            path: if self.world_backend == sqlite::NAME {
                self.world_path()
            } else {
                self.instance_dir.clone()
            },
            options: BackendOptions {
                sqlite_locking: self.sqlite_locking,
            },
        }
    }
}

fn usage() -> String {
    let backends = WORLD_BACKENDS.join("|");
    let lockings = Locking::ALL.map(Locking::name).join("|");
    let lockstep = TickLockstep::ALL.map(TickLockstep::name).join("|");
    format!(
        "usage: {} [{OFFLINE_FLAG}] [{WORLD_BACKEND_FLAG} <{backends}>] [{INSTANCE_DIR_FLAG} <dir>] \
         [{SERVER_FILE_FLAG} <file>] [{SQLITE_LOCKING_FLAG} <{lockings}>] \
         [{TICK_LOCKSTEP_FLAG} <{lockstep}>] [{SEED_FLAG} <seed>] \
         [{CONFIG_FLAG} <key> <value>]... [{DEFINE_FLAG}<key>=<value>]... [{NO_SERVE_FLAG}] [{NO_SAVE_FLAG}] [{PID_FILE_FLAG} <path>] [{EXECUTE_FLAG} <file>]... [{CONVERT_TO_FLAG} <{backends}>] [{CONVERT_OUTPUT_FLAG} <path>] [{FORCE_UPGRADE_FLAG}] [{ERASE_CACHE_FLAG}]",
        env!("CARGO_BIN_NAME"),
    )
}

/// Parses the arguments after the program name. The instance directory
/// defaults to `cwd` and the world backend to `vanilla`, or `sqlite` when a
/// server file is named.
fn parse_args(raw: impl IntoIterator<Item = String>, cwd: PathBuf) -> Result<Args> {
    let mut offline = None;
    let mut world_backend = None;
    let mut instance_dir = None;
    let mut server_file = None;
    let mut sqlite_locking = None;
    let mut tick_lockstep = None;
    let mut seed = None;
    let mut convert_to = None;
    let mut convert_output = None;
    let mut force_upgrade = None;
    let mut erase_cache = None;
    let mut config = Vec::new();
    let mut no_serve = None;
    let mut no_save = None;
    let mut pid_file = None;
    let mut execute = Vec::new();
    let mut iter = raw.into_iter();
    while let Some(arg) = iter.next() {
        if arg == OFFLINE_FLAG {
            set_once(&mut offline, OFFLINE_FLAG, ())?;
        } else if arg == FORCE_UPGRADE_FLAG {
            set_once(&mut force_upgrade, FORCE_UPGRADE_FLAG, ())?;
        } else if arg == ERASE_CACHE_FLAG {
            set_once(&mut erase_cache, ERASE_CACHE_FLAG, ())?;
        } else if arg == NO_SERVE_FLAG {
            set_once(&mut no_serve, NO_SERVE_FLAG, ())?;
        } else if arg == NO_SAVE_FLAG {
            set_once(&mut no_save, NO_SAVE_FLAG, ())?;
        } else if let Some(path) = flag_value(&arg, PID_FILE_FLAG, &mut iter)? {
            set_once(&mut pid_file, PID_FILE_FLAG, PathBuf::from(path))?;
        } else if let Some(file) = flag_value(&arg, EXECUTE_FLAG, &mut iter)? {
            execute.push(file);
        } else if arg == CONFIG_FLAG {
            let key = iter.next().filter(|key| !key.starts_with("--"));
            let (Some(key), Some(value)) = (key, iter.next()) else {
                bail!("{CONFIG_FLAG} needs a key and a value\n{}", usage());
            };
            config.push((key, value));
        } else if let Some(pair) = arg.strip_prefix("--config=") {
            let Some((key, value)) = pair.split_once('=').filter(|(key, _)| !key.is_empty()) else {
                bail!(
                    "{CONFIG_FLAG}=<key>=<value> needs a key and a value\n{}",
                    usage()
                );
            };
            config.push((key.to_owned(), value.to_owned()));
        } else if arg == DEFINE_FLAG {
            let Some(pair) = iter.next().filter(|pair| !pair.starts_with("--")) else {
                bail!("{DEFINE_FLAG} needs <key>=<value>\n{}", usage());
            };
            config.push(parse_define(&pair)?);
        } else if let Some(pair) = arg.strip_prefix(DEFINE_FLAG) {
            config.push(parse_define(pair)?);
        } else if let Some(name) = flag_value(&arg, WORLD_BACKEND_FLAG, &mut iter)? {
            set_once(
                &mut world_backend,
                WORLD_BACKEND_FLAG,
                parse_backend(WORLD_BACKEND_FLAG, &name)?,
            )?;
        } else if let Some(dir) = flag_value(&arg, INSTANCE_DIR_FLAG, &mut iter)? {
            set_once(&mut instance_dir, INSTANCE_DIR_FLAG, PathBuf::from(dir))?;
        } else if let Some(file) = flag_value(&arg, SERVER_FILE_FLAG, &mut iter)? {
            set_once(&mut server_file, SERVER_FILE_FLAG, PathBuf::from(file))?;
        } else if let Some(name) = flag_value(&arg, SQLITE_LOCKING_FLAG, &mut iter)? {
            set_once(
                &mut sqlite_locking,
                SQLITE_LOCKING_FLAG,
                parse_locking(&name)?,
            )?;
        } else if let Some(name) = flag_value(&arg, TICK_LOCKSTEP_FLAG, &mut iter)? {
            set_once(
                &mut tick_lockstep,
                TICK_LOCKSTEP_FLAG,
                parse_tick_lockstep(&name)?,
            )?;
        } else if let Some(value) = flag_value(&arg, SEED_FLAG, &mut iter)? {
            set_once(&mut seed, SEED_FLAG, parse_seed(&value))?;
        } else if let Some(name) = flag_value(&arg, CONVERT_TO_FLAG, &mut iter)? {
            set_once(
                &mut convert_to,
                CONVERT_TO_FLAG,
                parse_backend(CONVERT_TO_FLAG, &name)?,
            )?;
        } else if let Some(path) = flag_value(&arg, CONVERT_OUTPUT_FLAG, &mut iter)? {
            set_once(
                &mut convert_output,
                CONVERT_OUTPUT_FLAG,
                PathBuf::from(path),
            )?;
        } else {
            bail!("unknown argument {arg:?}\n{}", usage());
        }
    }
    let world_backend = world_backend.unwrap_or(if server_file.is_some() {
        sqlite::NAME
    } else {
        vanilla::NAME
    });
    if world_backend != sqlite::NAME {
        for (flag, given) in [
            (SERVER_FILE_FLAG, server_file.is_some()),
            (SQLITE_LOCKING_FLAG, sqlite_locking.is_some()),
        ] {
            if given {
                bail!(
                    "{flag} needs {WORLD_BACKEND_FLAG} {}\n{}",
                    sqlite::NAME,
                    usage()
                );
            }
        }
    }
    if no_save.is_some() {
        for (flag, given) in [
            (FORCE_UPGRADE_FLAG, force_upgrade.is_some()),
            (CONVERT_TO_FLAG, convert_to.is_some()),
        ] {
            if given {
                bail!("{flag} cannot be used with {NO_SAVE_FLAG}\n{}", usage());
            }
        }
    }
    if erase_cache.is_some() && force_upgrade.is_none() {
        bail!("{ERASE_CACHE_FLAG} needs {FORCE_UPGRADE_FLAG}\n{}", usage());
    }
    if force_upgrade.is_some() && convert_output.is_some() {
        convert_to.get_or_insert(world_backend);
    }
    match (convert_to, &convert_output) {
        (Some(_), None) => bail!("{CONVERT_TO_FLAG} needs {CONVERT_OUTPUT_FLAG}\n{}", usage()),
        (None, Some(_)) => bail!("{CONVERT_OUTPUT_FLAG} needs {CONVERT_TO_FLAG}\n{}", usage()),
        _ => {}
    }
    let features = features::enabled_from(&config).map_err(|err| anyhow!("{err}\n{}", usage()))?;
    features::disabled_vanilla_from(&config).map_err(|err| anyhow!("{err}\n{}", usage()))?;
    Ok(Args {
        offline: offline.is_some(),
        world_backend,
        instance_dir: instance_dir.unwrap_or(cwd),
        server_file,
        sqlite_locking: sqlite_locking.unwrap_or_default(),
        tick_lockstep: tick_lockstep.unwrap_or_default(),
        seed,
        convert_to,
        convert_output,
        force_upgrade: force_upgrade.is_some(),
        erase_cache: erase_cache.is_some(),
        config,
        no_serve: no_serve.is_some(),
        no_save: no_save.is_some(),
        pid_file,
        execute,
        features,
    })
}

fn write_pid_file(path: &std::path::Path) -> Result<()> {
    std::fs::write(path, std::process::id().to_string())
        .with_context(|| format!("cannot write pid file {}", path.display()))
}

fn parse_locking(name: &str) -> Result<Locking> {
    Locking::from_name(name).with_context(|| {
        format!(
            "unknown {SQLITE_LOCKING_FLAG} {name:?}; expected one of {}\n{}",
            Locking::ALL.map(Locking::name).join(", "),
            usage(),
        )
    })
}

fn parse_tick_lockstep(name: &str) -> Result<TickLockstep> {
    TickLockstep::from_name(name).with_context(|| {
        format!(
            "unknown {TICK_LOCKSTEP_FLAG} {name:?}; expected one of {}\n{}",
            TickLockstep::ALL.map(TickLockstep::name).join(", "),
            usage(),
        )
    })
}

/// A seed as vanilla reads `level-seed`: a number if it parses as one,
/// otherwise the Java hash code of the text.
fn parse_seed(value: &str) -> i64 {
    value.parse().unwrap_or_else(|_| {
        i64::from(value.encode_utf16().fold(0i32, |hash, unit| {
            hash.wrapping_mul(31).wrapping_add(i32::from(unit))
        }))
    })
}

fn set_once<T>(slot: &mut Option<T>, flag: &str, value: T) -> Result<()> {
    if slot.replace(value).is_some() {
        bail!("{flag} given more than once\n{}", usage());
    }
    Ok(())
}

fn parse_backend(flag: &str, name: &str) -> Result<&'static str> {
    WORLD_BACKENDS
        .into_iter()
        .find(|backend| *backend == name)
        .with_context(|| {
            format!(
                "unknown {flag} {name:?}; expected one of {}\n{}",
                WORLD_BACKENDS.join(", "),
                usage(),
            )
        })
}

/// Runs `--convert_to` and exits instead of starting the server: converts
/// the world named by `args`'s other flags (the source) into `convert_to`
/// at `--convert_output` (the destination), then prints what moved.
fn run_convert(args: &Args, convert_to: &'static str) -> Result<()> {
    let source = args.convert_source();
    let destination = ConvertEndpoint {
        backend: convert_to,
        path: args
            .convert_output
            .clone()
            .expect("convert_output required by parse_args"),
        options: BackendOptions::default(),
    };
    let report = if args.force_upgrade {
        convert_offline_rewriting(&source, &destination, args.erase_cache)
    } else {
        convert_offline(&source, &destination)
    }
    .with_context(|| {
        format!(
            "converting {} ({}) to {} ({})",
            source.path.display(),
            source.backend,
            destination.path.display(),
            destination.backend,
        )
    })?;
    println!(
        "converted {} ({}) to {} ({}):",
        source.path.display(),
        source.backend,
        destination.path.display(),
        destination.backend,
    );
    println!("  {} document(s)", report.documents);
    println!(
        "  {} chunk(s), {} entity record(s) and {} point of interest record(s) across {} dimension(s)",
        report.chunks, report.entity_records, report.poi_records, report.dimensions,
    );
    println!("  {} config item(s)", report.config_items);
    println!("  {} extra file(s)", report.extra_files);
    Ok(())
}

/// Runs `--force_upgrade` without `--convert_output`: rewrites the world in
/// place, keeping the old one as `<world>.pre-upgrade`, then lets the
/// server start.
fn run_force_upgrade(args: &Args) -> Result<()> {
    let source = args.convert_source();
    let report = force_upgrade_in_place(&source, args.erase_cache)
        .with_context(|| format!("upgrading {} ({})", source.path.display(), source.backend))?;
    println!(
        "upgraded {} chunk(s), {} entity record(s) and {} point of interest record(s) across {} dimension(s)",
        report.chunks, report.entity_records, report.poi_records, report.dimensions,
    );
    Ok(())
}

/// The value given as `<flag> <value>` or `<flag>=<value>`, if `arg` is
/// that flag.
fn flag_value(
    arg: &str,
    flag: &str,
    rest: &mut impl Iterator<Item = String>,
) -> Result<Option<String>> {
    let value = if arg == flag {
        rest.next().filter(|value| !value.starts_with("--"))
    } else if let Some(value) = arg
        .strip_prefix(flag)
        .and_then(|rest| rest.strip_prefix('='))
    {
        Some(value.to_owned()).filter(|value| !value.is_empty())
    } else {
        return Ok(None);
    };
    value
        .map(Some)
        .with_context(|| format!("{flag} needs a value\n{}", usage()))
}

fn main() {
    tuff_core::threads::init_rayon();
    let result = match tuff::allocator::runtime() {
        Ok(runtime) => runtime.block_on(run()),
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        eprintln!("Error: {error:?}");
        std::process::exit(tuff_core::exit_code::for_error(&error));
    }
}

async fn run() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some(datapack_cli::SUBCOMMAND) {
        print!("{}", datapack_cli::run(&raw[1..])?);
        return Ok(());
    }
    if raw.first().map(String::as_str) == Some(backup_cli::SUBCOMMAND) {
        print!("{}", backup_cli::run(&raw[1..])?);
        return Ok(());
    }
    if raw.first().map(String::as_str) == Some(plugin_cli::SUBCOMMAND) {
        print!("{}", plugin_cli::run(&raw[1..])?);
        return Ok(());
    }
    if raw.first().map(String::as_str) == Some(replay_cli::SUBCOMMAND) {
        logging::init();
        return replay_cli::run(&raw[1..]).await;
    }
    if raw.first().map(String::as_str) == Some(trace_cli::SUBCOMMAND) {
        return trace_cli::run(&raw[1..], &mut std::io::stdout().lock());
    }
    if raw.first().map(String::as_str) == Some(tuff::gametest_cli::SUBCOMMAND) {
        logging::init();
        let options = tuff::gametest_cli::parse(&raw[1..])?;
        let status = tuff::gametest_cli::run(options).await?;
        std::process::exit(status);
    }
    logging::init();
    let cwd = std::env::current_dir().context("no current directory")?;
    let args = parse_args(raw, cwd).context(tuff_core::exit_code::ArgumentError)?;

    if let Some(path) = &args.pid_file {
        write_pid_file(path)?;
    }

    if let Some(convert_to) = args.convert_to {
        return run_convert(&args, convert_to);
    }

    if !args.no_save {
        std::fs::create_dir_all(&args.instance_dir)
            .with_context(|| format!("cannot create {}", args.instance_dir.display()))?;
    }
    if args.force_upgrade {
        run_force_upgrade(&args)?;
    }
    let logs = LogHub::spawn(if args.no_save {
        HubOptions::default()
    } else {
        HubOptions::new(args.instance_dir.join("logs"))
    });
    logging::install(&logs);
    if !args.no_save {
        tuff_core::crash_report::set_directory(args.instance_dir.join("crash-reports"));
    }
    logging::install_panic_hook();
    if args.no_save {
        tuff_core::log::warn!(
            "{NO_SAVE_FLAG}: saving is disabled; the world, configuration and logs are not written to disk"
        );
    }

    let mut feature_config = features::saved_overrides(
        args.world_backend,
        &args.instance_dir,
        &args.world_path(),
        args.sqlite_locking,
        args.no_save,
    );
    feature_config.extend(args.config.iter().cloned());
    let enabled = features::enabled_from(&feature_config).map_err(|err| anyhow!(err))?;
    let disabled = features::disabled_vanilla_from(&feature_config).map_err(|err| anyhow!(err))?;
    let feature_list =
        features::shipped_without(&enabled, &disabled).map_err(|err| anyhow!(err))?;
    let loaded: Vec<&str> = feature_list
        .iter()
        .map(|feature| feature.id())
        .filter(|id| id.starts_with("vanilla-") || *id == "core-commands")
        .collect();
    tuff_core::log::info!("vanilla features loaded: {}", loaded.join(", "));
    for id in &enabled {
        tuff_core::log::info!("enabled feature {id}");
    }

    let server = Server::builder()
        .motd(motd(Duration::ZERO))
        .maybe_online_mode(args.offline.then_some(false))
        .maybe_view_distance(Some(64))
        .maybe_world_path(args.configured_world_path())
        .storage_backend(args.world_backend)
        .backend_options(BackendOptions {
            sqlite_locking: args.sqlite_locking,
        })
        .instance_dir(args.instance_dir)
        .tick_lockstep(args.tick_lockstep)
        .config_overrides(args.config)
        .no_save(args.no_save)
        .serve(!args.no_serve)
        .maybe_seed(args.seed)
        .features(feature_list)
        .build();

    let handle = server.handle();
    tuff::allocator::spawn_trimmer(handle.clone());
    let motd_handle = handle.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            if motd_handle.set_motd(motd(started.elapsed())).await.is_err() {
                break;
            }
        }
    });
    let console_handle = handle.clone();
    let stop_handle = handle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tuff_core::log::info!("stopping the server");
            let _ = stop_handle.shutdown().await;
        }
    });
    let scripts = args.execute;
    let no_serve = args.no_serve;
    let scripted = !scripts.is_empty();
    let failed = Arc::new(AtomicBool::new(false));
    let script_failed = failed.clone();
    tokio::spawn(async move {
        if scripted {
            let ok = run_script_files(&handle, &scripts).await;
            script_failed.store(!ok, Ordering::Relaxed);
        }
        if no_serve && scripted {
            let _ = handle.shutdown().await;
            return;
        }
        let lines = console::spawn_input(console_handle.clone(), tokio::runtime::Handle::current());
        console::run(console_handle, lines).await;
        if no_serve {
            let _ = handle.shutdown().await;
        }
    });

    let result = server.run().await;
    logs.shutdown().await;
    result?;
    match tuff_core::restart::action(tuff_core::restart::Relaunch::current) {
        tuff_core::restart::Action::Nothing => {}
        tuff_core::restart::Action::Exit(code) => std::process::exit(code),
        tuff_core::restart::Action::Relaunch(plan) => {
            tuff_core::log::info!("restarting: {}", plan.exe.display());
            let err = plan.exec();
            tuff_core::log::error!("restart failed: {err}");
            std::process::exit(tuff_core::restart::EXIT_CODE);
        }
    }
    if failed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
    Ok(())
}

/// Prints each script's feedback; false if the server stopped or any script
/// had a failing line.
async fn run_script_files(handle: &ServerHandle, files: &[String]) -> bool {
    match tuff::scripts::run_scripts(handle, files).await {
        Ok(reports) => {
            logging::flush().await;
            let mut all_ok = true;
            for report in reports {
                for line in &report.feedback {
                    println!("{line}");
                }
                all_ok &= report.ok;
            }
            all_ok
        }
        Err(_) => false,
    }
}

fn motd(uptime: Duration) -> TextComponent {
    TextComponent::new("Tuff, a Rust Minecraft Server")
        .color("gold")
        .bold(true)
        .append(
            TextComponent::new(format!("\nUp for {}s", uptime.as_secs()))
                .color("gray")
                .bold(false),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        parse_args(args.iter().map(|a| a.to_string()), PathBuf::from("/cwd"))
    }

    fn parse_err(args: &[&str]) -> String {
        let err = parse(args).unwrap_err().to_string();
        assert!(err.contains(&usage()), "{err}");
        err
    }

    #[test]
    fn usage_names_the_flags() {
        assert!(
            usage().ends_with(
                "[--offline] [--world_backend <vanilla|sqlite>] [--instance_dir <dir>] \
                 [--server_file <file>] [--sqlite_locking <exclusive|normal>] \
                 [--tick_lockstep <sprint|always>] [--seed <seed>] [--config <key> <value>]... [-D<key>=<value>]... [--no-serve] [--no-save] [--pid_file <path>] [--execute <file>]... [--convert_to <vanilla|sqlite>] [--convert_output <path>] [--force_upgrade] [--erase_cache]"
            ),
            "{}",
            usage()
        );
    }

    #[test]
    fn accepted_backends_are_builtin() {
        for backend in WORLD_BACKENDS {
            assert!(
                tuff_core::storage::BUILTIN
                    .iter()
                    .any(|(name, _)| *name == backend),
                "{backend}"
            );
        }
    }

    #[test]
    fn defaults_to_online_vanilla_in_cwd() {
        let args = parse(&[]).unwrap();
        assert_eq!(
            args,
            Args {
                offline: false,
                world_backend: vanilla::NAME,
                instance_dir: PathBuf::from("/cwd"),
                server_file: None,
                sqlite_locking: Locking::Normal,
                tick_lockstep: TickLockstep::Always,
                seed: None,
                convert_to: None,
                convert_output: None,
                force_upgrade: false,
                erase_cache: false,
                config: Vec::new(),
                no_serve: false,
                no_save: false,
                pid_file: None,
                execute: Vec::new(),
                features: Vec::new(),
            }
        );
        assert_eq!(args.world_path(), PathBuf::from("/cwd/world"));
    }

    #[test]
    fn tick_lockstep_flag() {
        for args in [
            ["--tick_lockstep", "always"].as_slice(),
            &["--tick_lockstep=always"],
        ] {
            assert_eq!(parse(args).unwrap().tick_lockstep, TickLockstep::Always);
        }
        let err = parse_err(&["--tick_lockstep", "never"]);
        assert!(err.contains("expected one of sprint, always"), "{err}");
    }

    #[test]
    fn seed_flag() {
        for args in [["--seed", "-42"].as_slice(), &["--seed=-42"]] {
            assert_eq!(parse(args).unwrap().seed, Some(-42));
        }
        assert_eq!(
            parse(&["--seed", "9223372036854775807"]).unwrap().seed,
            Some(i64::MAX)
        );
        assert_eq!(parse(&["--seed", "hello"]).unwrap().seed, Some(99162322));
        assert_eq!(
            parse(&["--seed=abcdefghij"]).unwrap().seed,
            Some(-634_317_659)
        );
        let err = parse_err(&["--seed", "1", "--seed", "2"]);
        assert!(err.contains("--seed given more than once"), "{err}");
        let err = parse_err(&["--seed"]);
        assert!(err.contains("--seed needs a value"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn pid_file_flag_names_a_file_that_gets_the_process_id() {
        let args = parse(&["--pid_file", "/run/tuff.pid"]).unwrap();
        assert_eq!(args.pid_file, Some(PathBuf::from("/run/tuff.pid")));
        parse_err(&["--pid_file", "a", "--pid_file=b"]);
        let dir = std::env::temp_dir().join(format!("tuff-pid-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tuff.pid");
        write_pid_file(&path).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_save_flag() {
        assert!(parse(&["--no-save"]).unwrap().no_save);
        assert!(!parse(&[]).unwrap().no_save);
        parse_err(&["--no-save", "--no-save"]);
        let err = parse_err(&["--no-save", "--force_upgrade"]);
        assert!(err.contains("cannot be used with --no-save"), "{err}");
    }

    #[test]
    fn no_serve_and_execute_flags() {
        let args = parse(&["--no-serve", "--execute", "a.txt", "--execute=b.txt"]).unwrap();
        assert!(args.no_serve);
        assert_eq!(args.execute, ["a.txt", "b.txt"]);
        let err = parse_err(&["--no-serve", "--no-serve"]);
        assert!(err.contains("--no-serve given more than once"), "{err}");
        let err = parse_err(&["--execute"]);
        assert!(err.contains("--execute needs a value"), "{err}");
    }

    #[test]
    fn offline_flag() {
        assert!(parse(&["--offline"]).unwrap().offline);
    }

    #[test]
    fn world_backend_in_both_forms() {
        for args in [
            ["--world_backend", "sqlite"].as_slice(),
            &["--world_backend=sqlite"],
        ] {
            let args = parse(args).unwrap();
            assert_eq!(args.world_backend, sqlite::NAME);
            assert_eq!(args.world_path(), PathBuf::from("/cwd/world.mcserver"));
        }
        for args in [
            ["--world_backend", "vanilla"].as_slice(),
            &["--world_backend=vanilla"],
        ] {
            let args = parse(args).unwrap();
            assert_eq!(args.world_backend, vanilla::NAME);
            assert_eq!(args.world_path(), PathBuf::from("/cwd/world"));
        }
    }

    #[test]
    fn instance_dir_in_both_forms() {
        for args in [
            ["--instance_dir", "/srv/mc"].as_slice(),
            &["--instance_dir=/srv/mc"],
        ] {
            let args = parse(args).unwrap();
            assert_eq!(args.instance_dir, PathBuf::from("/srv/mc"));
            assert_eq!(args.world_path(), PathBuf::from("/srv/mc/world"));
        }
    }

    #[test]
    fn flags_combine_in_any_order() {
        let args = parse(&[
            "--instance_dir=srv",
            "--offline",
            "--world_backend",
            "sqlite",
        ])
        .unwrap();
        assert_eq!(
            args,
            Args {
                offline: true,
                world_backend: sqlite::NAME,
                instance_dir: PathBuf::from("srv"),
                server_file: None,
                sqlite_locking: Locking::Normal,
                tick_lockstep: TickLockstep::Always,
                seed: None,
                convert_to: None,
                convert_output: None,
                force_upgrade: false,
                erase_cache: false,
                config: Vec::new(),
                no_serve: false,
                no_save: false,
                pid_file: None,
                execute: Vec::new(),
                features: Vec::new(),
            }
        );
        assert_eq!(args.world_path(), PathBuf::from("srv/world.mcserver"));
    }

    #[test]
    fn invalid_backend_lists_the_valid_ones() {
        for args in [
            ["--world_backend", "memory"].as_slice(),
            &["--world_backend=SQLITE"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("expected one of vanilla, sqlite"), "{err}");
        }
    }

    #[test]
    fn removed_flags_are_rejected() {
        for args in [
            ["--sqlite", "world.db"].as_slice(),
            &["--sqlite=world.db"],
            &["--instance-dir", "/srv"],
            &["--instance-dir=/srv"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("unknown argument"), "{err}");
        }
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        for args in [
            ["--bogus"].as_slice(),
            &["--world-backend", "sqlite"],
            &["--instance_dirx=/srv"],
            &["--world_backendsqlite"],
            &["/srv"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("unknown argument"), "{err}");
        }
    }

    #[test]
    fn duplicate_flags_are_rejected() {
        for args in [
            ["--offline", "--offline"].as_slice(),
            &["--world_backend=vanilla", "--world_backend", "vanilla"],
            &["--instance_dir", "a", "--instance_dir=b"],
            &["--server_file=a", "--server_file", "b"],
            &["--sqlite_locking=normal", "--sqlite_locking=normal"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("more than once"), "{err}");
        }
    }

    #[test]
    fn config_flags_repeat_and_keep_order() {
        let args = parse(&[
            "--config",
            "tuff.http.port",
            "8123",
            "--offline",
            "--config=tuff.http.host=::",
            "--config",
            "tuff.http.port",
            "9000",
        ])
        .unwrap();
        assert_eq!(
            args.config,
            vec![
                ("tuff.http.port".to_owned(), "8123".to_owned()),
                ("tuff.http.host".to_owned(), "::".to_owned()),
                ("tuff.http.port".to_owned(), "9000".to_owned()),
            ]
        );
        assert!(args.offline);
    }

    #[tuff_core::watchdog]
    #[test]
    fn define_flags_share_the_config_path() {
        let args = parse(&[
            "-Dtuff.http.port=8123",
            "-D",
            "pvp=false",
            "-Dminecraft.max-players=50",
            "--config",
            "pvp",
            "true",
            "-Dtuff.http.port=9000",
        ])
        .unwrap();
        let pair = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        assert_eq!(
            args.config,
            vec![
                pair("tuff.http.port", "8123"),
                pair("pvp", "false"),
                pair("minecraft.max-players", "50"),
                pair("pvp", "true"),
                pair("tuff.http.port", "9000"),
            ]
        );
    }

    #[test]
    fn define_flags_need_a_key_and_a_value() {
        for args in [
            ["-D"].as_slice(),
            &["-D", "--offline"],
            &["-D", "novalue"],
            &["-Dnovalue"],
            &["-D=x"],
        ] {
            parse_err(args);
        }
        assert!(parse(&["-Dtuff.http.port="]).is_ok());
    }

    #[test]
    fn define_flags_with_bad_keys_or_values_fail_startup_validation() {
        parse_err(&["-Dtuff.features.nope.enabled=true"]);
        parse_err(&["-Dtuff.features.audit-log.enabled=maybe"]);
    }

    #[test]
    fn features_are_off_unless_configured() {
        let args = parse(&[]).unwrap();
        assert!(args.features.is_empty());
        assert!(args.config.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_feature_config_keys_select_features() {
        let key = features::enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        let args = parse(&["--config", &key, "true"]).unwrap();
        assert_eq!(
            args.features,
            [tuff_feature_builtin_moving_block_entities::ID]
        );
        assert_eq!(args.config, vec![(key.clone(), "true".to_owned())]);
        let args = parse(&[
            &format!("--config={key}=true"),
            &format!("--config={key}=false"),
        ])
        .unwrap();
        assert!(args.features.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_features_and_bad_values_are_rejected() {
        let err = parse_err(&["--config", "tuff.features.nope.enabled", "true"]);
        assert!(err.contains("unknown feature \"nope\""), "{err}");
        let key = features::enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        let err = parse_err(&["--config", &key, "maybe"]);
        assert!(err.contains("true or false"), "{err}");
        let err = parse_err(&["--plugin", "x"]);
        assert!(err.contains("unknown argument"), "{err}");
    }

    #[test]
    fn config_flags_need_a_key_and_a_value() {
        for args in [
            ["--config"].as_slice(),
            &["--config", "tuff.http.port"],
            &["--config", "--offline", "x"],
            &["--config="],
            &["--config=novalue"],
            &["--config==x"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("needs a key and a value"), "{err}");
        }
    }

    #[test]
    fn missing_values_are_rejected() {
        for args in [
            ["--world_backend"].as_slice(),
            &["--world_backend="],
            &["--instance_dir"],
            &["--instance_dir="],
            &["--instance_dir", "--offline"],
            &["--server_file"],
            &["--server_file="],
            &["--sqlite_locking"],
            &["--sqlite_locking", "--offline"],
        ] {
            let err = parse_err(args);
            assert!(err.contains("needs a value"), "{err}");
        }
    }

    #[test]
    fn server_file_overrides_the_path_and_implies_sqlite() {
        for args in [
            ["--server_file", "/data/survival.mcserver"].as_slice(),
            &["--server_file=/data/survival.mcserver"],
            &[
                "--world_backend=sqlite",
                "--server_file=/data/survival.mcserver",
            ],
        ] {
            let args = parse(args).unwrap();
            assert_eq!(args.world_backend, sqlite::NAME);
            assert_eq!(args.world_path(), PathBuf::from("/data/survival.mcserver"));
        }
        let err = parse_err(&["--world_backend=vanilla", "--server_file=x.mcserver"]);
        assert!(
            err.contains("--server_file needs --world_backend sqlite"),
            "{err}"
        );
    }

    #[test]
    fn sqlite_locking_is_normal_unless_asked() {
        assert_eq!(
            parse(&["--world_backend=sqlite"]).unwrap().sqlite_locking,
            Locking::Normal
        );
        for (value, locking) in [
            ("exclusive", Locking::Exclusive),
            ("normal", Locking::Normal),
        ] {
            let args = parse(&["--world_backend=sqlite", "--sqlite_locking", value]).unwrap();
            assert_eq!(args.sqlite_locking, locking);
            let args = parse(&[&format!("--sqlite_locking={value}"), "--server_file=f"]).unwrap();
            assert_eq!(args.sqlite_locking, locking);
        }
        let err = parse_err(&["--sqlite_locking=normal"]);
        assert!(
            err.contains("--sqlite_locking needs --world_backend sqlite"),
            "{err}"
        );
        let err = parse_err(&["--world_backend=sqlite", "--sqlite_locking=shared"]);
        assert!(err.contains("expected one of exclusive, normal"), "{err}");
    }

    #[test]
    fn an_old_world_only_database_is_ignored() {
        let dir = std::env::temp_dir().join(format!("minecraft-main-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let args = parse(&[
            "--world_backend=sqlite",
            &format!("--instance_dir={}", dir.display()),
        ])
        .unwrap();
        assert_eq!(args.world_path(), dir.join("world.mcserver"));
        std::fs::write(dir.join("world.db"), b"").unwrap();
        assert_eq!(args.world_path(), dir.join("world.mcserver"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn convert_flags_in_both_forms() {
        for args in [
            ["--convert_to", "sqlite", "--convert_output", "out.mcserver"].as_slice(),
            &["--convert_to=sqlite", "--convert_output=out.mcserver"],
        ] {
            let args = parse(args).unwrap();
            assert_eq!(args.convert_to, Some(sqlite::NAME));
            assert_eq!(args.convert_output, Some(PathBuf::from("out.mcserver")));
        }
        let args = parse(&["--convert_to=vanilla", "--convert_output=/srv/out"]).unwrap();
        assert_eq!(args.convert_to, Some(vanilla::NAME));
    }

    #[test]
    fn convert_flags_need_each_other() {
        let err = parse_err(&["--convert_to=sqlite"]);
        assert!(err.contains("--convert_to needs --convert_output"), "{err}");
        let err = parse_err(&["--convert_output=out"]);
        assert!(err.contains("--convert_output needs --convert_to"), "{err}");
    }

    #[test]
    fn force_upgrade_rewrites_the_world_into_the_output_with_the_same_backend() {
        let args = parse(&["--force_upgrade", "--erase_cache", "--convert_output=out"]).unwrap();
        assert!(args.force_upgrade && args.erase_cache);
        assert_eq!(args.convert_to, Some(vanilla::NAME));
        let args = parse(&[
            "--world_backend=sqlite",
            "--force_upgrade",
            "--convert_output=out.mcserver",
        ])
        .unwrap();
        assert_eq!(args.convert_to, Some(sqlite::NAME));
        assert!(!args.erase_cache);
        let args = parse(&["--force_upgrade"]).unwrap();
        assert!(args.force_upgrade);
        assert_eq!(args.convert_to, None);
        assert_eq!(args.convert_output, None);
        let err = parse_err(&["--erase_cache"]);
        assert!(err.contains("--erase_cache needs --force_upgrade"), "{err}");
    }

    #[test]
    fn convert_to_rejects_unknown_backends() {
        let err = parse_err(&["--convert_to=memory", "--convert_output=out"]);
        assert!(err.contains("expected one of vanilla, sqlite"), "{err}");
    }

    #[test]
    fn convert_source_matches_the_backend_it_names() {
        let args = parse(&[
            "--instance_dir=/srv",
            "--convert_to=sqlite",
            "--convert_output=o",
        ])
        .unwrap();
        let source = args.convert_source();
        assert_eq!(source.backend, vanilla::NAME);
        assert_eq!(source.path, PathBuf::from("/srv"));

        let args = parse(&[
            "--world_backend=sqlite",
            "--server_file=/srv/world.mcserver",
            "--convert_to=vanilla",
            "--convert_output=o",
        ])
        .unwrap();
        let source = args.convert_source();
        assert_eq!(source.backend, sqlite::NAME);
        assert_eq!(source.path, PathBuf::from("/srv/world.mcserver"));
    }
}
