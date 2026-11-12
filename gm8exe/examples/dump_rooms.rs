//! Prints one line per room: its size, and how many instances, tiles, backgrounds and views it
//! starts with. Answers "is this game's frame cost coming from sheer room contents?" without
//! having to run it -- the engine draws every tile and steps every instance a room holds, with no
//! culling, so those two counts are the direct multiplier on a frame.
//!
//! Usage: cargo run --example dump_rooms -p gm8exe -- <path-to-exe>

use std::{fs, process};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = match args.next() {
        Some(p) => p,
        None => {
            eprintln!("usage: dump_rooms <path-to-exe>");
            process::exit(2);
        },
    };

    let mut file = fs::read(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {}: {}", path, e);
        process::exit(1);
    });
    let assets = gm8exe::reader::from_exe(&mut file, None::<fn(&str)>, true, false).unwrap_or_else(|e| {
        eprintln!("couldn't parse {}: {}", path, e);
        process::exit(1);
    });

    println!("{:<40} {:>11} {:>9} {:>7} {:>4}  {}", "room", "size", "instances", "tiles", "bgs", "view/port");
    let (mut max_inst, mut max_tiles) = (0, 0);
    let mut no_views = 0;
    for (i, room) in assets.rooms.iter().enumerate() {
        let room = match room {
            Some(r) => r,
            None => continue,
        };
        max_inst = max_inst.max(room.instances.len());
        max_tiles = max_tiles.max(room.tiles.len());
        // With views off, GameMaker's drawing region is the whole room -- so the room size is also
        // the framebuffer size, which is the thing worth seeing next to it.
        let views = if room.views_enabled {
            let visible: Vec<_> = room
                .views
                .iter()
                .filter(|v| v.visible)
                .map(|v| format!("{}x{} -> {}x{}", v.source_w, v.source_h, v.port_w, v.port_h))
                .collect();
            if visible.is_empty() { "enabled, none visible".to_string() } else { visible.join(", ") }
        } else {
            no_views += 1;
            "OFF (draws whole room)".to_string()
        };
        println!(
            "{:<40} {:>11} {:>9} {:>7} {:>4}  {}",
            format!("{}: {}", i, room.name),
            format!("{}x{}", room.width, room.height),
            room.instances.len(),
            room.tiles.len(),
            room.backgrounds.len(),
            views,
        );
    }
    println!("\nworst room: {} instances, {} tiles", max_inst, max_tiles);
    println!("{} of {} rooms have views off", no_views, assets.rooms.iter().flatten().count());
}
