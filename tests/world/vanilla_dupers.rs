//! Ground truth for the duplication contraptions in `dupe_designs.rs`: each
//! one is built and run on a real vanilla server (the jar the vanilla-data
//! build caches), offline, on a throwaway port-0 instance driven over its
//! console. Opt-in: skipped unless `TUFF_VANILLA_TESTS=1` and
//! `TUFF_ACCEPT_MOJANG_EULA=1`, so it never runs in `cargo t`.

use super::dupe_designs::{COUNT_BOX, DESIGNS, Design, ORIGIN, Part};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

pub type Counts = BTreeMap<String, u32>;

fn enabled() -> bool {
    let on = |k: &str| std::env::var(k).is_ok_and(|v| v == "1" || v == "true");
    on("TUFF_VANILLA_TESTS") && on("TUFF_ACCEPT_MOJANG_EULA")
}

fn cache_dir() -> PathBuf {
    std::env::var_os("VANILLA_CACHE_DIR")
        .map(PathBuf::from)
        .expect("VANILLA_CACHE_DIR names the vanilla-data build cache")
}

struct Vanilla {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    marker: u32,
    _dir: tuff_test_support::TestDir,
}

impl Vanilla {
    fn start() -> Self {
        let cache = cache_dir();
        let jar = cache.join("26.3/server.jar");
        let java = std::env::var_os("VANILLA_JAVA")
            .map(PathBuf::from)
            .unwrap_or_else(|| cache.join("java/java-runtime-epsilon/bin/java"));
        let dir = tuff_test_support::TestDir::new();
        std::fs::write(dir.path().join("eula.txt"), "eula=true\n").unwrap();
        std::fs::write(
            dir.path().join("server.properties"),
            "online-mode=false\nserver-port=0\nenable-status=false\nlevel-type=minecraft:flat\n\
             spawn-protection=0\nview-distance=4\nsimulation-distance=4\n\
             management-server-enabled=false\nspawn-monsters=false\nallow-nether=false\n",
        )
        .unwrap();
        let mut child = Command::new(java)
            .args(["-Xmx1G", "-jar"])
            .arg(&jar)
            .arg("--nogui")
            .current_dir(dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start the vanilla server");
        let stdin = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let vanilla = Self {
            child,
            stdin,
            lines,
            marker: 0,
            _dir: dir,
        };
        vanilla.wait_for("Done (");
        vanilla
    }

    fn wait_for(&self, needle: &str) -> Vec<String> {
        let limit = Instant::now() + Duration::from_secs(180);
        let mut seen = Vec::new();
        loop {
            let left = limit.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("vanilla never printed {needle:?}: {seen:?}"));
            let done = line.contains(needle);
            seen.push(line);
            if done {
                return seen;
            }
        }
    }

    /// Runs one command and returns what it answered.
    fn run(&mut self, command: &str) -> String {
        self.marker += 1;
        let mark = format!("@@{}", self.marker);
        writeln!(self.stdin, "{command}").unwrap();
        writeln!(self.stdin, "say {mark}").unwrap();
        self.stdin.flush().unwrap();
        let seen = self.wait_for(&format!("[Server] {mark}"));
        if std::env::var_os("TUFF_VANILLA_TRACE").is_some() {
            eprintln!("> {command}\n{seen:#?}");
        }
        seen.iter()
            .filter_map(|l| l.split_once("System chat: ").map(|(_, rest)| rest))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn game_time(&mut self) -> u64 {
        let answer = self.run("time query gametime");
        answer
            .split_whitespace()
            .find_map(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("game time from {answer:?}"))
    }

    fn step(&mut self, ticks: u32) {
        let before = self.game_time();
        self.run(&format!("tick step {ticks}"));
        let limit = Instant::now() + Duration::from_secs(60);
        while self.game_time() < before + u64::from(ticks) {
            assert!(Instant::now() < limit, "the server never stepped");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn place(&mut self, (dx, dy, dz, state): (i32, i32, i32, &str)) {
        let (x, y, z) = (ORIGIN.0 + dx, ORIGIN.1 + dy, ORIGIN.2 + dz);
        let fall = state.strip_prefix("@fall:").map(|block| {
            format!("@cmd:summon falling_block {{x}}.5 {{y}}.0 {{z}}.5 {{BlockState:{{Name:\"{block}\"}},Time:1}}")
        });
        let state = fall.as_deref().unwrap_or(state);
        if let Some(command) = state.strip_prefix("@cmd:") {
            let command = command
                .replace("{x}", &x.to_string())
                .replace("{y}", &y.to_string())
                .replace("{z}", &z.to_string());
            self.run(&command);
            return;
        }
        self.run(&format!("setblock {x} {y} {z} {state}"));
    }

    fn entities(&mut self, selector: &str) -> u32 {
        let answer = self.run(&format!("execute if entity {selector}"));
        answer
            .split("ount: ")
            .nth(1)
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    }

    fn counts(&mut self, design: &Design) -> Counts {
        let mut counts = Counts::new();
        let primed = self.entities("@e[type=minecraft:tnt]");
        if primed > 0 {
            counts.insert("primed_tnt".to_owned(), primed);
        }
        let (lo, hi) = COUNT_BOX;
        let o = ORIGIN;
        let (a, b) = (
            (o.0 + lo.0, o.1 + lo.1, o.2 + lo.2),
            (o.0 + hi.0, o.1 + hi.1, o.2 + hi.2),
        );
        for &kind in design.kinds {
            let mut total = 0;
            for held in 1..=16 {
                let stacks = self.entities(&format!(
                    "@e[type=minecraft:item,nbt={{Item:{{id:\"{kind}\",count:{held}}}}}]"
                ));
                total += stacks * held;
            }
            let items = total;
            total += self.entities(&format!(
                "@e[type=minecraft:falling_block,nbt={{BlockState:{{Name:\"{kind}\"}}}}]"
            ));
            if std::env::var_os("TUFF_VANILLA_DETAIL").is_some() {
                eprintln!("  detail {kind}: items {items}, total {total}");
            }
            if total > 0 {
                counts.insert(kind.to_owned(), total);
            }
        }
        let first = self.block_pass(design, a, b, 40);
        let second = self.block_pass(design, a, b, 50);
        assert_eq!(
            first, second,
            "counting the blocks disturbed the contraption"
        );
        for (key, n) in first {
            *counts.entry(key).or_default() += n;
        }
        counts
    }

    /// Standing blocks by id (and on the End platform as `end:<id>`),
    /// counted by cloning the area aside.
    fn block_pass(
        &mut self,
        design: &Design,
        a: (i32, i32, i32),
        b: (i32, i32, i32),
        aside: i32,
    ) -> Counts {
        let cloned_count = |text: &str| {
            text.split("cloned ")
                .nth(1)
                .and_then(|n| n.split(' ').next())
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        };
        let mut counts = Counts::new();
        for &kind in design.kinds {
            let text = self.run(&format!(
                "clone {} {} {} {} {} {} {} {} {} filtered {kind}",
                a.0,
                a.1,
                a.2,
                b.0,
                b.1,
                b.2,
                a.0,
                a.1,
                a.2 + aside
            ));
            if cloned_count(&text) > 0 {
                counts.insert(kind.to_owned(), cloned_count(&text));
            }
            if design.end {
                let text = self.run(&format!(
                    "execute in minecraft:the_end run clone 94 44 -8 106 56 8 94 44 {} filtered {kind}",
                    60 + aside
                ));
                if cloned_count(&text) > 0 {
                    counts.insert(format!("end:{kind}"), cloned_count(&text));
                }
            }
        }
        counts
    }

    /// Builds the design, settles it, fires the trigger and counts before
    /// and after.
    fn run_design(&mut self, design: &Design) -> (Counts, Counts) {
        let (x, z) = (ORIGIN.0, ORIGIN.2);
        self.run(&format!(
            "forceload add {} {} {} {}",
            x - 40,
            z - 40,
            x + 40,
            z + 90
        ));
        if design.end {
            self.run("execute in minecraft:the_end run forceload add 90 -10 110 120");
        }
        self.run("tick freeze");
        self.step(2);
        let (lo, hi) = COUNT_BOX;
        let o = ORIGIN;
        self.run(&format!(
            "fill {} {} {} {} {} {} minecraft:air",
            o.0 + lo.0 - 2,
            o.1 + lo.1 - 2,
            o.2 + lo.2 - 2,
            o.0 + hi.0 + 2,
            o.1 + hi.1 + 2,
            o.2 + hi.2 + 2
        ));
        for &(x, y, z, state) in design.build {
            let strict = format!("{} strict", state.trim_end_matches(" strict"));
            self.place((x, y, z, &strict));
        }
        self.step(6);
        let built = built_counts(design);
        for &part in design.trigger {
            match part.3.strip_prefix("@step:") {
                Some(n) => self.step(n.parse().unwrap()),
                None => self.place(part),
            }
        }
        self.step(design.steps);
        let after = self.counts(design);
        (built, after)
    }
}

impl Drop for Vanilla {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "stop");
        let _ = self.stdin.flush();
        let limit = Instant::now() + Duration::from_secs(20);
        while Instant::now() < limit {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
    }
}

/// What the build placed, counted from the list: counting on the server
/// before the trigger would disturb the contraption (a clone of the area
/// changed a later rail count).
pub fn built_counts(design: &Design) -> Counts {
    let mut counts = Counts::new();
    for &(_, _, _, state) in design.build.iter().chain(
        design
            .trigger
            .iter()
            .filter(|part| part.3.starts_with("@fall:")),
    ) {
        let id = state
            .trim_start_matches("@fall:")
            .split(['[', ' '])
            .next()
            .unwrap();
        if design.kinds.contains(&id) {
            *counts.entry(id.to_owned()).or_default() += 1;
        }
    }
    counts
}

fn table(name: &str, built: &Counts, after: &Counts) -> String {
    format!("{name}: built {built:?} after {after:?}")
}

#[tuff_core::watchdog]
#[test]
fn vanilla_counts_match_the_recorded_ones() {
    if !enabled() {
        eprintln!("skipped: set TUFF_VANILLA_TESTS=1 and TUFF_ACCEPT_MOJANG_EULA=1");
        return;
    }
    let only = std::env::var("TUFF_VANILLA_ONLY").ok();
    let mut failures = Vec::new();
    for design in DESIGNS {
        if only.as_deref().is_some_and(|n| n != design.name) {
            continue;
        }
        let mut vanilla = Vanilla::start();
        let (built, after) = vanilla.run_design(design);
        eprintln!("{}", table(design.name, &built, &after));
        drop(vanilla);
        let wanted: Counts = design
            .vanilla
            .iter()
            .map(|&(k, n)| (k.to_owned(), n))
            .collect();
        if wanted != after {
            failures.push(table(design.name, &built, &after));
        }
    }
    assert!(
        failures.is_empty(),
        "vanilla differs from the recorded counts:\n{}",
        failures.join("\n")
    );
}

/// Exploration: runs the layout in the file `TUFF_VANILLA_SCRIPT` names
/// (lines `kinds id,id`, `b dx dy dz state`, `t dx dy dz state`, `s ticks`;
/// `b`/`t` are build/trigger parts) and prints the counts.
#[tuff_core::watchdog]
#[test]
fn vanilla_script_layout() {
    let Some(path) = std::env::var_os("TUFF_VANILLA_SCRIPT").filter(|_| enabled()) else {
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    for (n, chunk) in text.split("\n---\n").enumerate() {
        let (mut kinds, mut build, mut trigger) = (Vec::new(), Vec::new(), Vec::new());
        let mut probes = Vec::new();
        let mut end = false;
        let leak = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        for line in chunk
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let mut w = line.split_whitespace();
            match w.next().unwrap() {
                "kinds" => kinds = w.next().unwrap().split(',').map(leak).collect(),
                "end" => end = true,
                "q" => {
                    let c: Vec<i32> = (0..3).map(|_| w.next().unwrap().parse().unwrap()).collect();
                    probes.push((c, w.next().unwrap().to_owned()));
                }
                "s" => trigger.push((0, 0, 0, leak(&format!("@step:{}", w.next().unwrap())))),
                tag @ ("b" | "t") => {
                    let c: Vec<i32> = (0..3).map(|_| w.next().unwrap().parse().unwrap()).collect();
                    let part = (c[0], c[1], c[2], leak(&w.collect::<Vec<_>>().join(" ")));
                    if tag == "b" {
                        build.push(part)
                    } else {
                        trigger.push(part)
                    }
                }
                other => panic!("unknown line {other}"),
            }
        }
        let design = Design {
            name: "script",
            build: Box::leak(build.into_boxed_slice()),
            trigger: Box::leak(trigger.into_boxed_slice()),
            steps: 4,
            end,
            kinds: Box::leak(kinds.into_boxed_slice()),
            vanilla: &[],
        };
        let mut vanilla = Vanilla::start();
        let (built, after) = vanilla.run_design(&design);
        eprintln!("{}", table(&format!("script#{n}"), &built, &after));
        for (c, id) in probes {
            let o = ORIGIN;
            let r = vanilla.run(&format!(
                "execute if block {} {} {} {id}",
                o.0 + c[0],
                o.1 + c[1],
                o.2 + c[2]
            ));
            eprintln!("script#{n} probe {c:?} {id}: {r}");
        }
    }
}
