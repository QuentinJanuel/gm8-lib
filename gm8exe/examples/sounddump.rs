//! Prints a GameMaker 8 game's sound assets: name, file type, kind and size.
//!
//! Which of them hold the multimedia slot is what `game_code`'s `sounds` reports to a mod, and
//! this is how that was checked against real games.
//!
//! Usage: cargo run --release --example sounddump -p gm8exe -- <path-to-exe>

fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let logger: Option<fn(&str)> = None;
    let assets = gm8exe::reader::from_exe(&mut bytes, logger, false, true).unwrap();
    for (i, s) in assets.sounds.iter().enumerate() {
        if let Some(s) = s {
            println!(
                "{:3} {:20} ext={:8} kind={} vol={} bytes={}",
                i,
                String::from_utf8_lossy(s.name.0.as_ref()),
                String::from_utf8_lossy(s.extension.0.as_ref()),
                s.kind as u32,
                s.volume,
                s.data.as_ref().map(|d| d.len()).unwrap_or(0),
            );
        }
    }
    println!("--- extensions: {}", assets.extensions.len());
    for ext in &assets.extensions {
        println!("ext {}", String::from_utf8_lossy(ext.name.0.as_ref()));
    }
}
