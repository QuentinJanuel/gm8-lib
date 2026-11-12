//! Prints a GameMaker 8 game's code in the mod loader's `GameCode` format, for checking the mapping
//! against a real game by eye.
//!
//! Usage: cargo run --release --example dump_game_code -p gm8exe -- <path-to-exe> [output.json]

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: dump_game_code <exe> [output.json]");
    let mut bytes = std::fs::read(path).expect("could not read the exe");
    let started = Instant::now();
    let assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, false).expect("could not parse the exe");
    let parsed = started.elapsed();
    let json = gm8exe::game_code::game_code_json(&assets);
    eprintln!(
        "parsed in {:?}, mapped in {:?}, {} bytes of JSON, pro edition: {}",
        parsed,
        started.elapsed() - parsed,
        json.len(),
        assets.pro_flag
    );
    match args.get(2) {
        Some(out) => std::fs::write(out, json).expect("could not write the output"),
        None => println!("{}", json),
    }
}
