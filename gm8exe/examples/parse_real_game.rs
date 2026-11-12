//! Ad-hoc verification harness: parse real GM8 .exe files end to end.
use std::{env, fs};

fn main() {
    let mut failures = 0;
    for path in env::args().skip(1) {
        let data = fs::read(&path).expect("read exe");
        match gm8exe::reader::from_exe(data, None::<fn(&str)>, true, true) {
            Ok(assets) => {
                let sprites = assets.sprites.iter().filter(|s| s.is_some()).count();
                let sounds = assets.sounds.iter().filter(|s| s.is_some()).count();
                let objects = assets.objects.iter().filter(|s| s.is_some()).count();
                let rooms = assets.rooms.iter().filter(|s| s.is_some()).count();
                let backgrounds = assets.backgrounds.iter().filter(|s| s.is_some()).count();
                println!("OK   {path}");
                println!(
                    "     sprites={sprites} sounds={sounds} \
                     backgrounds={backgrounds} objects={objects} rooms={rooms}"
                );
            },
            Err(e) => {
                failures += 1;
                println!("FAIL {path}\n     {e}");
            },
        }
    }
    if failures > 0 {
        std::process::exit(1);
    }
}
