//! Applies a list of insertions to a real game and prints the code they landed in.
//!
//! The loader's own path, with the mod host taken out: `game_code` writes the tokens, a mod picks
//! some, and `install_inserts` splices. Here the picking is a JSON file, so a mod's plan can be
//! checked against a real game's bytes without a browser.
//!
//! `apply_inserts <game.exe> <inserts.json> [<entry name to print> ...]`, where the JSON is a list
//! of `{ "at": ..., "place": ..., "call": ... }`.

use gm8exe::mods::ModInsert;

fn main() {
    let mut args = std::env::args().skip(1);
    let exe = args.next().expect("usage: apply_inserts <game.exe> <inserts.json> [entry ...]");
    let plan = args.next().expect("usage: apply_inserts <game.exe> <inserts.json> [entry ...]");
    let show: Vec<String> = args.collect();

    let mut bytes = std::fs::read(exe).unwrap();
    let mut assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, false).unwrap();

    let plan: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(plan).unwrap()).unwrap();
    let inserts: Vec<ModInsert> = plan
        .as_array()
        .unwrap()
        .iter()
        .map(|one| ModInsert {
            at: one["at"].as_str().unwrap().to_string(),
            place: one["place"].as_str().unwrap().to_string(),
            call: one["call"].as_str().unwrap().to_string(),
        })
        .collect();
    println!("applying {} insertion(s)", inserts.len());
    gm8exe::mods::install_inserts(&mut assets, inserts).unwrap();

    let text = |s: &gm8exe::asset::PascalString| String::from_utf8_lossy(&s.0).into_owned();
    for want in &show {
        let (room, rest) = want.split_once('.').unwrap_or((want.as_str(), ""));
        let found = assets
            .rooms
            .iter()
            .flatten()
            .find(|r| r.name.0.as_ref() == room.as_bytes())
            .unwrap_or_else(|| panic!("no room {room}"));
        if rest.is_empty() {
            for (slot, instance) in found.instances.iter().enumerate() {
                if !instance.creation_code.0.is_empty() {
                    println!("  {room}[{slot}] id={}: {:?}", instance.id, text(&instance.creation_code));
                }
            }
        } else {
            let id: i32 = rest.trim_start_matches("instance_").parse().unwrap();
            let instance = found.instances.iter().find(|i| i.id == id).unwrap();
            println!("  {want}: {:?}", text(&instance.creation_code));
        }
    }
}
