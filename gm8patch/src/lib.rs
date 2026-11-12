//! The GameMaker 8 mod patcher, as one wasm export.
//!
//! It needs no DOM, GL or audio, so it loads under plain Node as well as in a browser, in
//! milliseconds, with nothing in it that can fail for reasons unrelated to patching.
//!
//! Everything it does lives in `gm8exe`. This file is only the boundary.

use gm8exe::mods::{ModHook, ModInsert, ModObject, ModScript, event_numbers, patch_exe};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

/// One event of a mod's object, by the mod contract's name (`"create"`, `"beginStep"`, ...). See
/// `gm8exe::mods::event_numbers`.
#[derive(Deserialize)]
struct JsEvent {
    event: String,
    source: String,
}

#[derive(Deserialize)]
struct JsObject {
    name: String,
    depth: i32,
    persistent: bool,
    visible: bool,
    events: Vec<JsEvent>,
}

#[derive(Deserialize)]
struct JsScript {
    name: String,
    source: String,
}

#[derive(Deserialize)]
struct JsInsert {
    at: String,
    place: String,
    call: String,
}

#[derive(Deserialize)]
struct JsHook {
    script: String,
    original: String,
    source: String,
}

/// The game's code in the mod loader's `GameCode` format, as JSON. See `gm8exe::game_code`.
///
#[wasm_bindgen]
pub fn game_code_gm8(exe: &[u8]) -> Result<String, JsError> {
    let mut bytes = exe.to_vec();
    let assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, false)
        .map_err(|e| JsError::new(&format!("game code: could not read this game: {}", e)))?;
    Ok(gm8exe::game_code::game_code_json(&assets))
}

/// Patches a GameMaker 8 executable so it carries `scripts`, `inserts`, `hooks`, `objects` and the
/// mod channel itself -- see `gm8exe::mods::patch_exe`.
///
/// The two lists are JSON rather than `js_sys::Array`s so this crate needs no web bindings at all,
/// which is what lets it load under Node without a DOM. They arrive separately because they are
/// installed separately, scripts first -- see `gm8exe::mods::patch_exe`.
#[wasm_bindgen]
pub fn patch_gm8_exe(
    exe: &[u8],
    scripts_json: &str,
    inserts_json: &str,
    hooks_json: &str,
    objects_json: &str,
) -> Result<Vec<u8>, JsError> {
    let parsed_scripts: Vec<JsScript> = serde_json::from_str(scripts_json)
        .map_err(|e| JsError::new(&format!("mod patch: could not read the script list: {}", e)))?;
    let scripts = parsed_scripts.into_iter().map(|s| ModScript { name: s.name, source: s.source }).collect();

    let parsed_inserts: Vec<JsInsert> = serde_json::from_str(inserts_json)
        .map_err(|e| JsError::new(&format!("mod patch: could not read the insertion list: {}", e)))?;
    let inserts = parsed_inserts.into_iter().map(|i| ModInsert { at: i.at, place: i.place, call: i.call }).collect();

    let parsed_hooks: Vec<JsHook> = serde_json::from_str(hooks_json)
        .map_err(|e| JsError::new(&format!("mod patch: could not read the hook list: {}", e)))?;
    let hooks = parsed_hooks
        .into_iter()
        .map(|h| ModHook { script: h.script, original: h.original, source: h.source })
        .collect();

    let parsed: Vec<JsObject> = serde_json::from_str(objects_json)
        .map_err(|e| JsError::new(&format!("mod patch: could not read the object list: {}", e)))?;

    let mut objects = Vec::with_capacity(parsed.len());
    for o in parsed {
        let mut events = Vec::with_capacity(o.events.len());
        for e in o.events {
            let (list, sub) = event_numbers(&e.event)
                .ok_or_else(|| JsError::new(&format!("mod object {}: unknown event \"{}\"", o.name, e.event)))?;
            events.push((list, sub, e.source));
        }
        objects.push(ModObject { name: o.name, depth: o.depth, persistent: o.persistent, visible: o.visible, events });
    }

    patch_exe(exe, scripts, inserts, hooks, objects).map_err(|e| JsError::new(&e))
}
