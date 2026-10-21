//! `parity-diff VANILLA_EVENTS TUFF_EVENTS`: diffs the raw vanilla
//! `events.txt` against tuff's normalized event lines.
use std::process::ExitCode;

use tuff_bot::parity;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [vanilla, tuff] = args.as_slice() else {
        eprintln!("usage: parity-diff VANILLA_EVENTS TUFF_EVENTS");
        return ExitCode::from(2);
    };
    let read = |p: &String| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    let tuff_lines: Vec<String> = read(tuff).lines().map(str::to_owned).collect();
    let mismatches = parity::diff(&parity::parse_vanilla(&read(vanilla)), &tuff_lines);
    for m in &mismatches {
        println!("{m}");
    }
    println!("{} mismatches", mismatches.len());
    ExitCode::from(u8::from(!mismatches.is_empty()))
}
