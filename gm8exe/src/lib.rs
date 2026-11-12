#![allow(clippy::cognitive_complexity)]
#![allow(clippy::unreadable_literal)]

macro_rules! log {
    ($logger: expr, $x: expr) => {
        if let Some(logger) = &$logger {
            logger($x.into());
        }
    };
    ($logger: expr, $format: expr, $($x: expr),*) => {
        if let Some(logger) = &$logger {
            logger(&format!(
                $format,
                $($x),*
            ));
        }
    };
    ($($x:expr,)*) => (log![$($x),*]); // leveraged from vec![]
}

pub mod asset;
pub mod builtin_names;
pub mod def;
pub mod game_code;
pub mod gamedata;
pub mod mods;
pub mod reader;
pub mod rsrc;
pub mod settings;
pub mod upx;
pub mod writer;

mod colour;

use crate::asset::{PascalString, *};
use settings::{GameHelpDialog, Settings};

pub type AssetList<T> = Vec<Option<Box<T>>>;

pub struct GameAssets {
    pub triggers: AssetList<Trigger>,
    pub constants: Vec<Constant>,
    pub extensions: Vec<Extension>,
    pub sprites: AssetList<Sprite>,
    pub sounds: AssetList<Sound>,
    pub backgrounds: AssetList<Background>,
    pub paths: AssetList<Path>,
    pub scripts: AssetList<Script>,
    pub fonts: AssetList<Font>,
    pub timelines: AssetList<Timeline>,
    pub objects: AssetList<Object>,
    pub rooms: AssetList<Room>,
    pub included_files: Vec<IncludedFile>,
    pub version: GameVersion,

    /// The name the embedded DirectX DLL is extracted under at runtime, e.g. `D3DX8.dll`.
    ///
    /// Carried rather than discarded because the runner writes the DLL out under this name and then
    /// loads it by name: a game shipping D3DX9 but relabelled D3DX8 fails at DirectX init.
    pub dx_dll_name: PascalString,
    pub dx_dll: Vec<u8>,
    pub ico_file_raw: Option<Vec<u8>>,
    pub help_dialog: GameHelpDialog,
    pub last_instance_id: i32,
    pub last_tile_id: i32,
    pub library_init_strings: Vec<PascalString>,
    pub room_order: Vec<i32>,

    /// Everything in the game data past the last section the reader parses.
    ///
    /// In an 8.0 or 8.1 game this is the run of random dwords GameMaker leaves at the end and
    /// nothing ever reads. In a GameMaker 8.2 game it is a length-prefixed section of 8.2's own,
    /// which this crate does not parse -- and dropping it produces a game that will not start.
    /// Carried whole so the writer can put back exactly what was there, parsed or not.
    pub trailing: Vec<u8>,

    pub settings: Settings,
    /// Whether the game was built with the Pro edition. Affects what the runner permits.
    pub pro_flag: bool,
    pub game_id: u32,
    pub guid: [u32; 4],
}

#[derive(Copy, Clone, Debug)]
pub enum GameVersion {
    GameMaker8_0,
    GameMaker8_1,
}

pub use colour::Colour;

#[cfg(test)]
mod every_statement {
    /// The games to check, from `GM8_CHECK_GAMES`: `:`-separated paths to GameMaker 8 executables.
    ///
    /// An environment variable rather than a list in this file, because the games are not ours to
    /// check in and every machine keeps them somewhere else. This is why the test is `#[ignore]`d
    /// rather than quietly passing with nothing to check.
    fn games() -> Vec<String> {
        std::env::var("GM8_CHECK_GAMES")
            .unwrap_or_default()
            .split(':')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Every piece of GML in a game, with the name to blame if it stops parsing.
    fn sources(assets: &crate::GameAssets) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = assets
            .scripts
            .iter()
            .flatten()
            .map(|s| (String::from_utf8_lossy(s.name.0.as_ref()).into_owned(), s.source.0.to_vec()))
            .collect();
        for object in assets.objects.iter().flatten() {
            let name = String::from_utf8_lossy(object.name.0.as_ref()).into_owned();
            for (list, subs) in object.events.iter().enumerate() {
                for (sub, actions) in subs {
                    for (index, action) in actions.iter().enumerate() {
                        // The two buffers that hold GML, by the same rule `game_code::Mapper::
                        // actions` reads them: a "code" action's first parameter, and an
                        // extension action's own code. Every other action's parameters are
                        // expressions and arguments, which are not GML and never parsed.
                        let source = match (action.action_kind, action.execution_type) {
                            (7, _) => action.param_strings[0].0.to_vec(),
                            (0, 2) => action.fn_code.0.to_vec(),
                            _ => continue,
                        };
                        out.push((format!("{name}.{list}_{sub}#{index}"), source));
                    }
                }
            }
        }
        for room in assets.rooms.iter().flatten() {
            let name = String::from_utf8_lossy(room.name.0.as_ref()).into_owned();
            out.push((format!("{name}.room"), room.creation_code.0.to_vec()));
            for (slot, instance) in room.instances.iter().enumerate() {
                out.push((format!("{name}.instance[{slot}]"), instance.creation_code.0.to_vec()));
            }
        }
        out
    }

    /// Inserting at *every* addressable statement of *every* script, event and piece of room code
    /// leaves all of them parsing -- including `"instead"`, which takes statements away.
    ///
    /// The check the plan calls for at M3, and the one that catches a splice landing in the wrong
    /// place: a script cut mid-statement stops parsing, where the unit tests' all-ASCII fixtures
    /// would never notice. `"instead"` is in here because removing a statement is the one edit that
    /// makes the source *shorter*, which every offset recorded after it has to survive.
    #[test]
    #[ignore]
    fn inserting_everywhere_leaves_every_script_parsing() {
        let games = games();
        assert!(!games.is_empty(), "set GM8_CHECK_GAMES to `:`-separated paths of GameMaker 8 .exe files");
        for place in ["after", "before", "instead"] {
            for path in &games {
                if !std::path::Path::new(path).exists() {
                    println!("skipped (not here): {path}");
                    continue
                }
                let mut bytes = std::fs::read(path).unwrap();
                let mut assets = crate::reader::from_exe(&mut bytes, None::<fn(&str)>, false, false).unwrap();

                // What parsed before, so code that never parsed is not blamed on us.
                let broken_before: Vec<String> = sources(&assets)
                    .into_iter()
                    .filter(|(_, source)| gml_parser::ast::AST::new(source).is_err())
                    .map(|(name, _)| name)
                    .collect();

                let code = crate::game_code::game_code(&assets);
                let mut tokens: Vec<String> = Vec::new();
                fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
                    if let Some(a) = v.as_array() {
                        a.iter().for_each(|i| walk(i, out));
                        return
                    }
                    let Some(f) = v.as_object() else { return };
                    if let Some(serde_json::Value::String(t)) = f.get("at") {
                        out.push(t.clone());
                    }
                    for k in ["then", "else", "body", "bodies"] {
                        if let Some(n) = f.get(k) {
                            walk(n, out);
                        }
                    }
                }
                for s in code["scripts"].as_array().unwrap() {
                    walk(&s["body"], &mut tokens);
                }
                for e in code["events"].as_array().unwrap() {
                    walk(&e["body"], &mut tokens);
                }
                for r in code["rooms"].as_array().unwrap() {
                    walk(&r["body"], &mut tokens);
                }

                let inserts: Vec<crate::mods::ModInsert> = tokens
                    .iter()
                    .map(|t| crate::mods::ModInsert { at: t.clone(), place: place.to_string(), call: "m_ins".into() })
                    .collect();
                let count = inserts.len();
                crate::mods::install_inserts(&mut assets, inserts).unwrap();

                let mut broken = Vec::new();
                for (name, source) in sources(&assets) {
                    if broken_before.contains(&name) {
                        continue
                    }
                    if let Err(e) = gml_parser::ast::AST::new(&source) {
                        broken.push(format!("{name}: {}", e.message));
                    }
                }
                let game = path.rsplit('/').next().unwrap();
                println!("{place:>6} {count:>5} statements  {game}  (already broken: {})", broken_before.len());
                assert!(broken.is_empty(), "{place} on {game} broke {} scripts: {:?}", broken.len(), broken);
            }
        }
    }
}
