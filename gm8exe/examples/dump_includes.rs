//! Lists every Include File embedded in a GM8 exe: its name, whether the exe actually carries its
//! data, and how big that data is.
//!
//! GM8.2's `include_file_*` family reads these by name, so when a game stops at "trying to get
//! nonexisting included file(x)", this answers the first question -- is `x` an Include File at all,
//! and is it embedded, or was it only ever a path on the developer's machine?
//!
//! Usage: cargo run --example dump_includes -p gm8exe -- <path-to-exe>

use std::fs;

fn main() {
    let exe_path = std::env::args().nth(1).expect("usage: dump_includes <exe>");

    let mut bytes = fs::read(&exe_path).expect("failed to read exe");
    let assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, true).expect("failed to parse exe");

    println!("{} include file(s)", assets.included_files.len());
    for (i, file) in assets.included_files.iter().enumerate() {
        println!(
            "{i:4}  {:<40} data={} stored={} declared={} embedded={}",
            file.file_name.to_string(),
            file.data_exists,
            file.stored_in_gmk,
            file.source_length,
            file.embedded_data.as_ref().map_or(0, |d| d.len()),
        );
    }
}
