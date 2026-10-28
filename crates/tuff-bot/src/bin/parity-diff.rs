//! `parity-diff VANILLA_EVENTS TUFF_EVENTS [TUFF_TICKETS]`: diffs the raw
//! vanilla `events.txt` against tuff's normalized event lines, and, when given
//! tuff's `tuff-tickets.txt`, the forceloaded chunk sets as well.
use std::process::ExitCode;

use tuff_bot::parity;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (vanilla, tuff, tickets) = match args.as_slice() {
        [v, t] => (v, t, None),
        [v, t, k] => (v, t, Some(k)),
        _ => {
            eprintln!("usage: parity-diff VANILLA_EVENTS TUFF_EVENTS [TUFF_TICKETS]");
            return ExitCode::from(2);
        }
    };
    let read = |p: &String| std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"));
    let tuff_lines: Vec<String> = read(tuff).lines().map(str::to_owned).collect();
    let vanilla_text = read(vanilla);
    let mut mismatches = parity::diff(&parity::parse_vanilla(&vanilla_text), &tuff_lines);
    if let Some(tickets) = tickets {
        let tuff_tickets: Vec<String> = read(tickets).lines().map(str::to_owned).collect();
        mismatches.extend(parity::diff_tickets(
            &parity::vanilla_forced_tickets(&vanilla_text),
            &tuff_tickets,
        ));
    }
    for m in &mismatches {
        println!("{m}");
    }
    println!("{} mismatches", mismatches.len());
    ExitCode::from(u8::from(!mismatches.is_empty()))
}
