//! Installing mod resources into a parsed game.
//!
//! Splicing happens at the asset level, before anything is compiled or launched, which is what lets
//! mod GML be compiled by the same pass as the game's own and refer to the game's own assets by
//! name. It lives beside the reader and writer because its output is a patched executable, the same
//! file whether a real runner or an emulator plays it; nothing here knows an emulator exists.

use crate::{
    GameAssets,
    asset::{CodeAction, Object, PascalString, Script, code_action::PARAM_COUNT, extension::Extension, room},
};
use std::{collections::HashMap, io::Cursor};

/// GameMaker 8's event lists, by index. Mirrors `gm8emulator::gml::ev`, repeated rather than
/// imported because this crate must not depend on the engine.
pub mod ev {
    /// `applies_to` on an action: the instance running it. Mirrors `gm8emulator::gml::SELF`.
    pub const SELF: i32 = -1;
    pub const CREATE: usize = 0;
    pub const DESTROY: usize = 1;
    pub const STEP: usize = 3;
    pub const OTHER: usize = 7;
    pub const DRAW: usize = 8;
}

/// GM8 fixes the number of event lists on an object at 12; `gm8exe`'s reader rejects any other
/// count outright, and `Game::launch` zips the parsed lists against a `[_; 12]`. An injected object
/// has to look exactly like a parsed one.
const EVENT_LIST_COUNT: usize = 12;

/// The GM8 "Execute a piece of code" drag-and-drop action, as the IDE itself emits it: kind
/// `CODE`, with the GML in the first parameter slot. `action.rs`'s `Tree::from_list` compiles that
/// slot through the same `Compiler` the game's own events go through.
const ACTION_KIND_CODE: u32 = 7;
/// A "normal" action, which is every action that runs a library function -- and, when its
/// `execution_type` is code, one that carries its GML in `fn_code` instead. That is where an
/// extension's own actions keep theirs, and `game_code` reads it, so an insertion can land there.
const ACTION_KIND_NORMAL: u32 = 0;
/// GM8's action library id for the standard "move" library, which is where Execute Code lives.
/// Unread by the *emulator's* `Tree::from_list`; set to what the IDE writes, because the field
/// being unread by our own reader has already turned out twice to say nothing about the runner.
const ACTION_LIB_ID: u32 = 1;
/// The Execute Code action's id within that library. Same standing, same reasoning.
const ACTION_ID_CODE: u32 = 603;
/// "Run this action as code", which for kind `CODE` means the GML in the first parameter slot.
///
/// The emulator never reads this -- `action.rs` matches on `action_kind` first, and its
/// `execution_type` arm is `CODE | _`, so any value at all behaves as code. The real GameMaker 8
/// runner is not so forgiving: it reads `NONE` here and runs nothing, silently. A patched game
/// loads, starts, and simply has no mod in it. Every Execute Code action the GM8 IDE writes carries
/// 2, and `gm8decompiler`'s `fix_event` recognises one by exactly this pair of values.
const EXECUTION_TYPE_CODE: u32 = 2;
/// The argument type the IDE gives an Execute Code action's one slot: string, not expression.
///
/// Unread here for the same reason `execution_type` looked unread -- so it is set from what real
/// games contain rather than from what our own compiler happens to tolerate.
const PARAM_TYPE_STRING: u32 = 1;

/// One object a mod wants installed. Field names match the mod contract's `ObjectResource`.
pub struct ModObject {
    pub name: String,
    /// Lower depth draws later, i.e. on top. A HUD wants a large negative number.
    pub depth: i32,
    /// A non-persistent mod object would vanish at the first room change, which for a fangame is
    /// the first screen transition -- so this is `true` for anything that means to stay.
    pub persistent: bool,
    pub visible: bool,
    /// `(event list index, sub-event number, GML source)`. The indices are GM8's own: see
    /// `gml::ev` for the list, and note that sub-event 0/1/2 of `ev::STEP` are Step, Begin Step and
    /// End Step respectively.
    pub events: Vec<(usize, u32, String)>,
}

/// A mod event's name, as the mod contract spells it, in GameMaker 8's numbering:
/// `(event list index, sub-event number)`. `None` for a name the contract does not have.
///
/// GameMaker Studio numbers these differently, which is why the contract uses names.
pub fn event_numbers(name: &str) -> Option<(usize, u32)> {
    Some(match name {
        "create" => (0, 0),
        "destroy" => (1, 0),
        "step" => (3, 0),
        "beginStep" => (3, 1),
        "endStep" => (3, 2),
        "draw" => (8, 0),
        _ => return None,
    })
}

/// One script a mod wants installed. Field names match the mod contract's `ScriptResource`.
pub struct ModScript {
    pub name: String,
    /// Arguments arrive as `argument0`, `argument1`, ...; `return` returns.
    pub source: String,
}

/// One hook a mod wants installed. Field names match the mod contract's `HookResource`.
pub struct ModHook {
    /// The game script whose body is replaced.
    pub script: String,
    /// The new script the replaced body moves to.
    pub original: String,
    /// The new body of `script`.
    pub source: String,
}

/// One insertion a mod wants installed. Field names match the mod contract's `InsertResource`.
pub struct ModInsert {
    /// The statement to run around, as `<entry>#<route>,<start>,<end>`. Written by `game_code`,
    /// read back here, and opaque everywhere in between. See `Where`.
    pub at: String,
    /// `"before"`, `"after"` or `"instead"`.
    pub place: String,
    /// The mod script to call. Already installed, by `install_mod_scripts`.
    pub call: String,
}

/// Which source buffer a token points into.
///
/// A script has one. An object event has one per drag-and-drop action, which is why an action's
/// index is part of this and not of the entry name: two actions of one event are two buffers, and
/// an offset only means something together with the buffer it was measured in. A room has one for
/// itself and one per instance placed in it.
///
/// Everything but a script is found by index rather than by name, which is what `game_code` wrote
/// into the route for exactly this reason: an instance has no name, and an event's name would have
/// to be parsed back through a table of event lists this crate deliberately does not keep.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Where {
    Script(String),
    Event { object: usize, list: usize, sub: u32, action: usize },
    RoomCode { room: usize },
    InstanceCode { room: usize, instance: usize },
}

/// One statement's place: which buffer, and where in it.
struct Span {
    at: Where,
    start: usize,
    end: usize,
}

fn parse_at(at: &str) -> Result<Span, String> {
    let bad = || format!("mod install: {at:?} is not a statement this loader can insert around");
    let number = |text: &str| text.parse::<usize>().map_err(|_| bad());
    let (entry, route) = at.split_once('#').ok_or_else(bad)?;
    let fields: Vec<&str> = route.split(',').collect();
    let (at, start, end) = match fields.as_slice() {
        ["s", start, end] => (Where::Script(entry.to_string()), number(start)?, number(end)?),
        ["e", object, list, sub, action, start, end] => (
            Where::Event {
                object: number(object)?,
                list: number(list)?,
                sub: number(sub)? as u32,
                action: number(action)?,
            },
            number(start)?,
            number(end)?,
        ),
        ["r", room, start, end] => (Where::RoomCode { room: number(room)? }, number(start)?, number(end)?),
        ["i", room, instance, start, end] => {
            (Where::InstanceCode { room: number(room)?, instance: number(instance)? }, number(start)?, number(end)?)
        },
        _ => return Err(bad()),
    };
    if end < start {
        return Err(bad())
    }
    Ok(Span { at, start, end })
}

/// Everything a mod wants run at one statement.
struct Edit {
    span: Span,
    before: Vec<String>,
    after: Vec<String>,
    /// Calls that run in the statement's place, which is also what drops the statement.
    instead: Vec<String>,
}

/// Runs a mod's scripts at chosen statements inside the game's own, by editing the source.
///
/// GameMaker 8 compiles its GML when the game starts, so an insertion is a text edit made before
/// that: a statement `S` becomes `{S; call();}` for `"after"` and `{call(); S}` for `"before"`. The
/// braces are what make it safe in every position -- a braceless `if (x) a = 1 else b = 2` becomes
/// `if (x) {a = 1; call();} else b = 2` -- and braces have no scope in GML, so nothing else about
/// the script changes. The span already covers the statement's trailing `;` (see `AtExpr`), without
/// which `if (x) a = 1; else` would become `if (x) {...}; else` and stop compiling.
///
/// **Only a call is spliced, never the mod's own GML.** Pasting the body in would work here and be
/// less code, but it would hand GameMaker 8 mods the host's `var` locals, its arguments and an
/// `exit` that returns from the host script -- none of which the GameMaker Studio detour can offer.
/// The same mod would then quietly mean two different things. See `InsertResource`.
///
/// Installed before hooks, because `at` names offsets into the game's original source and a hook
/// moves that source wholesale.
pub fn install_inserts(assets: &mut GameAssets, inserts: Vec<ModInsert>) -> Result<(), String> {
    insert_into(&mut assets.scripts, &mut assets.objects, &mut assets.rooms, inserts)
}

/// The asset lists an insertion can land in, rather than the whole game: it is what lets a test
/// splice a script without building a `GameAssets`.
fn insert_into(
    scripts: &mut [Option<Box<Script>>],
    objects: &mut [Option<Box<Object>>],
    rooms: &mut [Option<Box<room::Room>>],
    inserts: Vec<ModInsert>,
) -> Result<(), String> {
    // Everything wanted at one statement becomes a single edit, rather than each wrapping the last.
    // Wrapping would reverse them: two "before" insertions applied in turn give `{B(); {A(); S}}`,
    // which runs B first -- and load order is the only thing a mod can use to sequence them.
    let mut edits: Vec<Edit> = Vec::new();
    for insert in inserts {
        let span = parse_at(&insert.at)?;
        let found =
            edits.iter().position(|e| e.span.at == span.at && e.span.start == span.start && e.span.end == span.end);
        let index = match found {
            Some(index) => index,
            None => {
                edits.push(Edit { span, before: Vec::new(), after: Vec::new(), instead: Vec::new() });
                edits.len() - 1
            },
        };
        match insert.place.as_str() {
            "before" => edits[index].before.push(insert.call),
            "after" => edits[index].after.push(insert.call),
            "instead" => edits[index].instead.push(insert.call),
            other => {
                return Err(format!(
                    "mod install: an insertion's place is \"before\", \"after\" or \"instead\", not {other:?}"
                ))
            },
        }
    }

    // Innermost and latest first. Statements nest but never partially overlap, so an edit already
    // applied is either wholly inside the next one or wholly after it -- which is what lets the
    // shift below be a sum rather than a remapping.
    edits.sort_by(|a, b| b.span.start.cmp(&a.span.start).then(a.span.end.cmp(&b.span.end)));

    // Per source buffer, the edits already made: where they were, and how much the text grew or
    // shrank there.
    let mut applied: HashMap<Where, Vec<(usize, usize, isize)>> = HashMap::new();

    for edit in edits {
        let Span { at, start, end } = edit.span;
        let done = applied.entry(at.clone()).or_default();
        // An edit already applied inside this statement moved its end; one after it did not, and
        // none can have moved its start, since they were all at a start at or beyond this one.
        let shift: isize = done.iter().filter(|(s, e, _)| *s >= start && *e <= end).map(|(_, _, grew)| grew).sum();
        let end_now = usize::try_from(end as isize + shift).map_err(|_| {
            format!("mod install: an earlier insertion into {} left nothing of the statement at {start}..{end}", at)
        })?;

        let name = at.to_string();
        // Bytes, not a `String`. GameMaker 8 source is not UTF-8 -- the Moon's `saveGame` opens
        // with a Shift-JIS comment -- and `from_utf8_lossy` replaces each invalid byte with a
        // three-byte U+FFFD. Offsets from the parser index the original bytes, so slicing a lossy
        // copy puts every edit somewhere else in the script, growing further wrong with each
        // replaced byte. That reads as a game whose code was mangled, not as an encoding problem.
        let buffer = source_of(scripts, objects, rooms, &at)?;
        let source = buffer.0.as_ref();
        if end_now > source.len() || start > end_now {
            return Err(format!(
                "mod install: the statement at {start}..{end} is not inside {name}, which is {} bytes. \
                 The game's code was read from a different copy of this game.",
                source.len()
            ))
        }

        let mut spliced: Vec<u8> = Vec::with_capacity(source.len() + 32);
        spliced.extend_from_slice(&source[..start]);
        spliced.push(b'{');
        for call in &edit.before {
            spliced.extend_from_slice(call.as_bytes());
            spliced.extend_from_slice(b"();");
        }
        if edit.instead.is_empty() {
            spliced.extend_from_slice(&source[start..end_now]);
        } else {
            // The statement is not copied over: what a mod asked to run in its place runs, and
            // what was there does not. The braces still stand in for it, so a braceless `else`
            // keeps its one statement and the script still compiles.
            for call in &edit.instead {
                spliced.extend_from_slice(call.as_bytes());
                spliced.extend_from_slice(b"();");
            }
        }
        // GML lets a statement end without one, so the separator is added rather than assumed. A
        // doubled `;` is an empty statement, which is legal and parses away.
        if !edit.after.is_empty() {
            spliced.push(b';');
            for call in &edit.after {
                spliced.extend_from_slice(call.as_bytes());
                spliced.extend_from_slice(b"();");
            }
        }
        spliced.push(b'}');
        spliced.extend_from_slice(&source[end_now..]);

        let grew = spliced.len() as isize - source.len() as isize;
        *buffer = PascalString(spliced.into_boxed_slice());
        applied.entry(at).or_default().push((start, end, grew));
    }
    Ok(())
}

impl std::fmt::Display for Where {
    /// How an error names the piece of code, which is how `game_code` named it: a script by name,
    /// an event the way the IDE labels it. The event list is not spelled out here -- this crate
    /// must not grow a second copy of that table -- so it says which list by number.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Where::Script(name) => write!(f, "{name}"),
            Where::Event { object, list, sub, action } => {
                write!(f, "object {object}'s event {list}.{sub}, action {action}")
            },
            Where::RoomCode { room } => write!(f, "room {room}'s creation code"),
            Where::InstanceCode { room, instance } => {
                write!(f, "room {room}'s instance {instance}, its creation code")
            },
        }
    }
}

/// The source buffer a token points into, to be edited in place.
///
/// Which of a code action's two buffers holds its GML is decided here by the same rule
/// `game_code::Mapper::actions` used to read it, rather than recorded in the token: one rule in two
/// places that must agree, not two rules that could drift.
fn source_of<'a>(
    scripts: &'a mut [Option<Box<Script>>],
    objects: &'a mut [Option<Box<Object>>],
    rooms: &'a mut [Option<Box<room::Room>>],
    at: &Where,
) -> Result<&'a mut PascalString, String> {
    match at {
        Where::Script(name) => scripts
            .iter_mut()
            .flatten()
            .find(|existing| existing.name.0.as_ref() == name.as_bytes())
            .map(|script| &mut script.source)
            .ok_or_else(|| format!("mod install: cannot insert into {name}, this game has no script by that name")),
        Where::Event { object, list, sub, action } => {
            let missing = || {
                format!(
                    "mod install: cannot insert into {at}, which this game does not have. \
                     The game's code was read from a different copy of this game."
                )
            };
            let object = objects.get_mut(*object).and_then(|o| o.as_mut()).ok_or_else(missing)?;
            let actions = object
                .events
                .get_mut(*list)
                .and_then(|subs| subs.iter_mut().find(|(s, _)| s == sub))
                .map(|(_, actions)| actions)
                .ok_or_else(missing)?;
            let action = actions.get_mut(*action).ok_or_else(missing)?;
            if action.action_kind == ACTION_KIND_CODE {
                Ok(&mut action.param_strings[0])
            } else if action.action_kind == ACTION_KIND_NORMAL && action.execution_type == EXECUTION_TYPE_CODE {
                Ok(&mut action.fn_code)
            } else {
                Err(format!("mod install: {at} is not GML this loader can edit"))
            }
        },
        Where::RoomCode { room } | Where::InstanceCode { room, .. } => {
            let missing = || {
                format!(
                    "mod install: cannot insert into {at}, which this game does not have. \
                     The game's code was read from a different copy of this game."
                )
            };
            let found = rooms.get_mut(*room).and_then(|r| r.as_mut()).ok_or_else(missing)?;
            match at {
                Where::InstanceCode { instance, .. } => {
                    Ok(&mut found.instances.get_mut(*instance).ok_or_else(missing)?.creation_code)
                },
                _ => Ok(&mut found.creation_code),
            }
        },
    }
}

/// Installs hooks, in chain order: each moves a script's current body to a new script and gives it
/// the hook's source instead.
///
/// **Moved, not copied.** The script asset keeps its index, so a stored index -- `script_execute`
/// on a variable -- reaches the new body exactly like a call by name does. The original body goes
/// to a new asset at the end of the list, which nothing in the game could have referred to by index.
///
/// A second hook on the same script finds the first hook's body there and moves *that*, which is
/// what makes several hooks chain instead of overwriting each other.
pub fn install_hooks(assets: &mut GameAssets, hooks: Vec<ModHook>) -> Result<(), String> {
    hook_scripts(&mut assets.scripts, hooks)
}

fn hook_scripts(scripts: &mut Vec<Option<Box<Script>>>, hooks: Vec<ModHook>) -> Result<(), String> {
    for hook in hooks {
        let taken = scripts.iter().flatten().any(|existing| existing.name.0.as_ref() == hook.original.as_bytes());
        if taken {
            return Err(format!(
                "mod install: cannot keep the original of {} as {}, a script by that name already exists",
                hook.script, hook.original
            ))
        }
        let target = scripts
            .iter_mut()
            .flatten()
            .find(|existing| existing.name.0.as_ref() == hook.script.as_bytes())
            .ok_or_else(|| {
            format!("mod install: cannot hook {}, this game has no script by that name", hook.script)
        })?;
        let body = std::mem::replace(&mut target.source, PascalString::from(hook.source.as_str()));
        scripts.push(Some(Box::new(Script { name: PascalString::from(hook.original.as_str()), source: body })));
    }
    Ok(())
}

/// Installs a mod's scripts, refusing any name the game -- or another mod -- already uses.
///
/// **The refusal is the point.** GameMaker resolves a call against scripts before builtins, and
/// `Game::launch` registers script names with `or_insert`, so a duplicate is not an error there:
/// the first registration wins and the loser is simply unreachable by name. A mod script named
/// after one of the game's own would therefore either do nothing or, on the engine that does
/// overwrite (see the GameMaker Studio side), silently replace the game's code. Neither is
/// something to discover while playing.
pub fn install_mod_scripts(assets: &mut GameAssets, scripts: Vec<ModScript>) -> Result<Vec<i32>, String> {
    let mut indices = Vec::with_capacity(scripts.len());
    for script in scripts {
        let taken = assets.scripts.iter().flatten().any(|existing| existing.name.0.as_ref() == script.name.as_bytes());
        if taken {
            return Err(format!(
                "mod install: this game already has a script called {}, and a mod may not replace one",
                script.name
            ))
        }
        indices.push(assets.scripts.len() as i32);
        assets.scripts.push(Some(Box::new(Script {
            name: PascalString::from(script.name.as_str()),
            source: PascalString::from(script.source.as_str()),
        })));
    }
    Ok(indices)
}

/// Where the first mod instance goes in the first room's instance list.
///
/// The front, ahead of the game's own -- but behind the channel controller when there is one, whose
/// Create defines the globals every mod object's Create may go on to call. See `install_objects`.
fn mod_instance_at(objects_in_room: impl Iterator<Item = i32>, channel: Option<i32>) -> usize {
    let Some(channel) = channel else { return 0 };
    objects_in_room.into_iter().position(|object| object == channel).map_or(0, |at| at + 1)
}

/// Installs each object into `assets` and places one instance of it in the game's first room.
///
/// Returns the object indices assigned, in the order given.
///
/// An object asset alone would never run: something has to instantiate it. Putting a single
/// instance in the first room and marking it persistent is how GM8 games have always done their
/// own "controller object", and it costs no engine changes at all -- by the time `Game::launch`
/// sees these assets they are indistinguishable from the game's.
pub fn install_objects(assets: &mut GameAssets, objects: Vec<ModObject>) -> Result<Vec<i32>, String> {
    if objects.is_empty() {
        return Ok(Vec::new())
    }

    // The first room is where an instance can be placed such that it exists from the very first
    // frame. `room_order` is the game's own play order, so its head is the room the player lands in
    // -- not necessarily room 0.
    let first_room = *assets.room_order.first().ok_or("mod install: this game has no rooms")?;

    // Mod instances go to the *front* of the room, not the end, matching the GameMaker Studio
    // backend's `PlaceFirst`. A fangame's controller is routinely first in its own room and wipes
    // the room on the way to the menu; appended last, a mod instance is created, marked for
    // destruction by that Create, and then skipped by every event dispatch afterwards.
    //
    // Behind the channel controller where there is one, though. `install_channel` front-loads it
    // for its own reason -- its Create defines the globals `host_send` reads, and every mod object's
    // Create may call that -- so the order has to be channel, then mods, then the game's own.
    let channel = assets
        .objects
        .iter()
        .position(|object| matches!(object, Some(o) if o.name.0.as_ref() == CHANNEL_OBJECT.as_bytes()))
        .map(|index| index as i32);
    let mut at = {
        let room = assets
            .rooms
            .get(first_room as usize)
            .and_then(|r| r.as_ref())
            .ok_or_else(|| format!("mod install: first room ({}) is missing", first_room))?;
        mod_instance_at(room.instances.iter().map(|i| i.object), channel)
    };

    let mut indices = Vec::with_capacity(objects.len());
    for object in objects {
        let index = assets.objects.len() as i32;
        let name = object.name.clone();

        let mut events: Vec<Vec<(u32, Vec<CodeAction>)>> = (0..EVENT_LIST_COUNT).map(|_| Vec::new()).collect();
        for (list, sub, source) in object.events {
            let list_events = events.get_mut(list).ok_or_else(|| {
                format!("mod install: object {} has an event in list {}, which does not exist", name, list)
            })?;
            list_events.push((sub, vec![code_action(&source)]));
        }

        assets.objects.push(Some(Box::new(Object {
            name: PascalString::from(name.as_str()),
            sprite_index: -1,
            solid: false,
            visible: object.visible,
            depth: object.depth,
            persistent: object.persistent,
            parent_index: -1,
            mask_index: -1,
            events,
        })));

        // `last_instance_id` is what the engine hands out for the next runtime-created instance, so
        // taking one from it here is what keeps a mod instance from colliding with a `instance_create`
        // later in the run.
        assets.last_instance_id += 1;
        let instance_id = assets.last_instance_id;

        let room = assets
            .rooms
            .get_mut(first_room as usize)
            .and_then(|r| r.as_mut())
            .ok_or_else(|| format!("mod install: first room ({}) is missing", first_room))?;
        room.instances.insert(at, room::Instance {
            x: 0,
            y: 0,
            object: index,
            id: instance_id,
            creation_code: PascalString::default(),
            xscale: 1.0,
            yscale: 1.0,
            blend: u32::MAX,
            angle: 0.0,
        });
        // Advanced so the objects keep the order they were given, rather than reversing.
        at += 1;

        indices.push(index);
    }

    Ok(indices)
}

pub(crate) fn code_action(source: &str) -> CodeAction {
    let mut param_strings: [PascalString; PARAM_COUNT] = Default::default();
    param_strings[0] = PascalString::from(source);
    CodeAction {
        id: ACTION_ID_CODE,
        // Some(SELF) and None execute identically (`action.rs`'s `exec_slice`); SELF is what the
        // IDE writes, so it is what an injected action writes too.
        applies_to: ev::SELF,
        is_condition: false,
        invert_condition: false,
        is_relative: false,
        lib_id: ACTION_LIB_ID,
        action_kind: ACTION_KIND_CODE,
        execution_type: EXECUTION_TYPE_CODE,
        can_be_relative: 0,
        applies_to_something: true,
        fn_name: PascalString::default(),
        fn_code: PascalString::default(),
        param_count: 1,
        param_types: {
            let mut types = [0; PARAM_COUNT];
            types[0] = PARAM_TYPE_STRING;
            types
        },
        param_strings,
    }
}

// -- The patched-executable channel ---------------------------------------------------------------
//
// A patched game is the same file whether a real runner or an emulator plays it, and the runner
// supplies nothing, so a patched game has to carry the whole channel itself:
// two GML scripts, one controller object, and a socket library, because GameMaker 8 has neither
// sockets nor buffers of its own -- zero `network_*` and zero `buffer_*` functions.
//
// The library is Http Dll 2.3, spliced in as a GameMaker 8 extension. See `vendor/README.md`.

/// The extension record, parsed at install time rather than hand-declared. Its 129 function
/// signatures would otherwise have to be transcribed by hand and kept in step with a binary we do
/// not build.
const HTTP_DLL_EXTENSION: &[u8] = include_bytes!("../vendor/http_dll8");

/// Http Dll 2.3's first terminal `socket_get_state` value, from `vendor/http-dll-2/sockets.txt`:
/// 4 is closed and 5 is error, and neither is recoverable.
const SOCKET_STATE_CLOSED: u32 = 4;

/// The command-line arguments a host passes the patched game to say where the channel listens.
const PORT_PREFIX: &str = "--emu-mod-port=";
const GAME_PREFIX: &str = "--emu-mod-game=";

pub const CHANNEL_OBJECT: &str = "mod_channel";
pub const SEND_SCRIPT: &str = "host_send";
pub const RECEIVE_SCRIPT: &str = "host_receive";

/// Sends one message. `argument0` is the text.
///
/// The buffer is cleared first and the socket flushed immediately, so a message sent during a
/// Create event -- before any step has run -- still leaves on the same frame.
fn send_script_source() -> String {
    format!(
        "if (global.{sock} < 0) exit;\n\
         buffer_clear(global.{tx});\n\
         buffer_write_string(global.{tx}, argument0);\n\
         socket_write_message(global.{sock}, global.{tx});\n\
         socket_update_write(global.{sock});\n",
        sock = "__mod_socket",
        tx = "__mod_txbuf",
    )
}

/// Pops one message, or returns "" when there is nothing waiting.
fn receive_script_source() -> String {
    "if (ds_list_size(global.__mod_inbox) == 0) return \"\";\n\
     global.__mod_rxmsg = ds_list_find_value(global.__mod_inbox, 0);\n\
     ds_list_delete(global.__mod_inbox, 0);\n\
     return global.__mod_rxmsg;\n"
        .to_string()
}

/// Reads the port and game id off the command line and dials the host.
///
/// `socket_connect` is asynchronous -- the state machine runs in `socket_update_*` -- so the hello
/// is queued here and actually leaves once the connection completes, which is why nothing waits on
/// a connected state before writing. Confirmed against the real 8.0 runner: Http Dll holds a
/// message written while the socket is still connecting and delivers it on connect.
///
/// The argument scan runs to `parameter_count()` *inclusive*. GameMaker 8 numbers arguments from 1
/// while `parameter_count()` counts them, so a `< count` loop reads the program slot and drops
/// whichever argument came last -- which was the game id, and the host was receiving `hello:` with
/// nothing after it. Reading one index past the end costs nothing; GM8 returns an empty string for
/// an argument that is not there.
fn channel_create_source() -> String {
    format!(
        "global.__mod_inbox = ds_list_create();\n\
         global.__mod_socket = -1;\n\
         global.__mod_txbuf = buffer_create();\n\
         global.__mod_rxbuf = buffer_create();\n\
         global.__mod_port = -1;\n\
         global.__mod_id = \"\";\n\
         global.__mod_i = 0;\n\
         while (global.__mod_i <= parameter_count()) {{\n\
         global.__mod_arg = parameter_string(global.__mod_i);\n\
         if (string_pos(\"{port}\", global.__mod_arg) == 1) global.__mod_port = real(string_delete(global.__mod_arg, 1, {port_len}));\n\
         if (string_pos(\"{game}\", global.__mod_arg) == 1) global.__mod_id = string_delete(global.__mod_arg, 1, {game_len});\n\
         global.__mod_i += 1;\n\
         }}\n\
         if (global.__mod_port > 0) {{\n\
         global.__mod_socket = socket_create();\n\
         socket_connect(global.__mod_socket, \"127.0.0.1\", global.__mod_port);\n\
         {send}(\"hello:\" + global.__mod_id);\n\
         }}\n",
        port = PORT_PREFIX,
        game = GAME_PREFIX,
        port_len = PORT_PREFIX.len(),
        game_len = GAME_PREFIX.len(),
        send = SEND_SCRIPT,
    )
}

/// Pumps the socket once a frame, drains whatever arrived, and retires the socket once it dies.
///
/// Begin Step, so a mod object's own Step sees this frame's messages rather than last frame's.
/// GameMaker 8 has no async networking event; polling is the only way in, and one pair of update
/// calls per frame costs nothing.
///
/// The state check is last so it sees this frame's updates, and it matters because Http Dll accepts
/// a write to a closed or errored socket and reports success: without it, a host that has gone away
/// -- the app quitting, a crash, a restart -- leaves every later `host_send` silently discarding its
/// message. Setting the handle back to -1 is what `host_send`'s own guard tests.
fn channel_step_source() -> String {
    format!(
        "if (global.__mod_socket >= 0) {{\n\
         socket_update_write(global.__mod_socket);\n\
         socket_update_read(global.__mod_socket);\n\
         buffer_clear(global.__mod_rxbuf);\n\
         while (socket_read_message(global.__mod_socket, global.__mod_rxbuf)) {{\n\
         buffer_set_pos(global.__mod_rxbuf, 0);\n\
         ds_list_add(global.__mod_inbox, buffer_read_string(global.__mod_rxbuf));\n\
         buffer_clear(global.__mod_rxbuf);\n\
         }}\n\
         if (socket_get_state(global.__mod_socket) >= {closed}) {{\n\
         socket_destroy(global.__mod_socket);\n\
         global.__mod_socket = -1;\n\
         }}\n\
         }}\n",
        closed = SOCKET_STATE_CLOSED,
    )
}

/// Closes the socket on the way out. GameMaker 8 has no CleanUp event; Game End is the equivalent.
fn channel_game_end_source() -> String {
    "if (global.__mod_socket >= 0) {\n\
     socket_destroy(global.__mod_socket);\n\
     global.__mod_socket = -1;\n\
     }\n"
    .to_string()
}

/// Adds the socket library, unless this game already carries it.
///
/// Reusing an existing copy rather than adding a second is not politeness: two extensions declaring
/// the same 129 function names collide, and the game fails to load.
pub fn install_extension(assets: &mut GameAssets) -> Result<bool, String> {
    let mut blob = HTTP_DLL_EXTENSION.to_vec();
    let mut cursor = Cursor::new(blob.as_mut_slice());
    let extension = Extension::read(&mut cursor, false)
        .map_err(|e| format!("mod install: the bundled socket extension is unreadable: {}", e))?;

    let already = assets.extensions.iter().any(|e| e.folder_name.0.as_ref() == extension.folder_name.0.as_ref());
    if already {
        return Ok(false)
    }
    assets.extensions.push(extension);
    Ok(true)
}

/// Adds `host_send` and `host_receive` as real GML scripts, returning their asset indices.
///
/// Installed before any mod's own scripts, which is what makes `install_mod_scripts` refuse a mod
/// that tried to define a script by either of these names.
///
/// Refuses a name the game itself already uses, for the reason `install_mod_scripts` gives: names
/// are registered with `or_insert`, so the game's own would win and every mod's send would compile
/// to the wrong body -- with the patch reporting success. Re-patching an already patched build
/// lands here too, which is the likely way to meet it.
pub fn install_scripts(assets: &mut GameAssets) -> Result<Vec<i32>, String> {
    let mut indices = Vec::new();
    for (name, source) in [(SEND_SCRIPT, send_script_source()), (RECEIVE_SCRIPT, receive_script_source())] {
        let taken = assets.scripts.iter().flatten().any(|existing| existing.name.0.as_ref() == name.as_bytes());
        if taken {
            return Err(format!(
                "mod install: this game already has a script called {}, so the mod channel cannot define one",
                name
            ))
        }
        indices.push(assets.scripts.len() as i32);
        assets.scripts.push(Some(Box::new(Script {
            name: PascalString::from(name),
            source: PascalString::from(source.as_str()),
        })));
    }
    Ok(indices)
}

/// Installs the whole channel: the library, the two scripts, and the controller object.
///
/// The controller's instance goes to the *front* of the first room's list so its Create runs before
/// any mod object's -- every one of those may call `host_send`, and the globals it reads are defined
/// here.
pub fn install_channel(assets: &mut GameAssets) -> Result<(), String> {
    install_extension(assets)?;
    install_scripts(assets)?;

    let first_room = *assets.room_order.first().ok_or("mod install: this game has no rooms")?;
    let index = assets.objects.len() as i32;

    let mut events: Vec<Vec<(u32, Vec<CodeAction>)>> = (0..EVENT_LIST_COUNT).map(|_| Vec::new()).collect();
    events[ev::CREATE].push((0, vec![code_action(&channel_create_source())]));
    // Sub-event 1 of the step list is Begin Step.
    events[ev::STEP].push((1, vec![code_action(&channel_step_source())]));
    // Sub-event 3 of the "other" list is Game End.
    events[ev::OTHER].push((3, vec![code_action(&channel_game_end_source())]));

    assets.objects.push(Some(Box::new(Object {
        name: PascalString::from(CHANNEL_OBJECT),
        sprite_index: -1,
        solid: false,
        visible: false,
        depth: 0,
        persistent: true,
        parent_index: -1,
        mask_index: -1,
        events,
    })));

    assets.last_instance_id += 1;
    let instance_id = assets.last_instance_id;
    let room = assets
        .rooms
        .get_mut(first_room as usize)
        .and_then(|r| r.as_mut())
        .ok_or_else(|| format!("mod install: first room ({}) is missing", first_room))?;
    room.instances.insert(0, room::Instance {
        x: 0,
        y: 0,
        object: index,
        id: instance_id,
        creation_code: PascalString::default(),
        xscale: 1.0,
        yscale: 1.0,
        blend: u32::MAX,
        angle: 0.0,
    });
    Ok(())
}

/// Reads a GameMaker 8 executable, installs the channel and a mod's objects, and writes it back.
///
/// The whole patcher in one call, so the only thing a caller has to know is the two byte arrays.
/// Ordering is the point of the function: the channel goes in first so its controller instance
/// leads the room and its Create defines the globals every mod object's own Create may read.
///
/// The game always carries the whole channel itself, over the socket extension, so the same patched
/// file runs on a real runner and on an emulator that answers that extension.
///
/// Handles GameMaker 8.0 and 8.1. An 8.1 runner checks its own game data and quits without a word
/// if any of it changed, so the writer disarms that check on the way out; see
/// [`crate::writer::disarm_gm81_data_check`].
pub fn patch_exe(
    original: &[u8],
    scripts: Vec<ModScript>,
    inserts: Vec<ModInsert>,
    hooks: Vec<ModHook>,
    objects: Vec<ModObject>,
) -> Result<Vec<u8>, String> {
    // The reader decrypts in place, and the writer wants that decrypted buffer rather than the
    // bytes on disk: a GameMaker 8.1 file's xor layer covers some of the run-up to the game data,
    // and those bytes are carried across as plaintext and re-encrypted with everything after them.
    let mut exe = original.to_vec();
    let mut assets = crate::reader::from_exe(&mut exe, None::<fn(&str)>, false, false)
        .map_err(|e| format!("mod patch: could not read this game: {}", e))?;

    // The channel needs functions GameMaker 8 reserves for Pro (the socket extension's, among
    // others), and a game built with the free edition refuses them at run time -- "This function is
    // only available in the Pro Edition." The flag is what the runner checks, so a patched game is a
    // Pro one. I Wanna Play Online patched games the same way.
    assets.pro_flag = true;
    install_channel(&mut assets)?;
    // Scripts before objects, so a name clash is refused before anything has been spliced in --
    // and to match the GameMaker Studio side, where the order is what makes a script call compile
    // as one.
    install_mod_scripts(&mut assets, scripts)?;
    // Insertions before hooks: their offsets name the game's original source, which a hook then
    // moves wholesale -- taking the edited body, and its insertions, with it.
    install_inserts(&mut assets, inserts)?;
    install_hooks(&mut assets, hooks)?;
    install_objects(&mut assets, objects)?;

    crate::writer::to_exe(&assets, &exe).map_err(|e| format!("mod patch: could not write the game: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(name: &str, source: &str) -> Option<Box<Script>> {
        Some(Box::new(Script { name: PascalString::from(name), source: PascalString::from(source) }))
    }

    fn body(scripts: &[Option<Box<Script>>], name: &str) -> String {
        let script = scripts.iter().flatten().find(|s| s.name.0.as_ref() == name.as_bytes()).unwrap();
        String::from_utf8_lossy(&script.source.0).into_owned()
    }

    fn hook(script: &str, original: &str) -> ModHook {
        ModHook { script: script.into(), original: original.into(), source: format!("return {}();", original) }
    }

    #[test]
    fn mod_instances_go_first_but_stay_behind_the_channel() {
        // No channel object (a game that was not given the channel). Mod instances take the very
        // front, ahead of the game's own
        // controller, so a first-room `with (all) instance_destroy()` cannot outrun them.
        assert_eq!(mod_instance_at([7, 8].into_iter(), None), 0);
        // A patched exe, where `install_channel` has already front-loaded its controller: directly
        // behind it, because `host_send`'s globals are defined in that Create.
        assert_eq!(mod_instance_at([9, 7, 8].into_iter(), Some(9)), 1);
        // The object exists but no instance of it was placed: nothing to stay behind.
        assert_eq!(mod_instance_at([7, 8].into_iter(), Some(9)), 0);
    }

    #[test]
    fn hooks_move_the_body_and_chain_in_order() {
        let mut scripts = vec![script("saveGame", "save_the_game();")];
        hook_scripts(&mut scripts, vec![hook("saveGame", "a_orig"), hook("saveGame", "b_orig")]).unwrap();

        // The script keeps its index; the bodies move to the end of the list.
        assert_eq!(scripts[0].as_ref().unwrap().name.0.as_ref(), b"saveGame");
        assert_eq!(body(&scripts, "saveGame"), "return b_orig();");
        assert_eq!(body(&scripts, "b_orig"), "return a_orig();");
        assert_eq!(body(&scripts, "a_orig"), "save_the_game();");
    }

    #[test]
    fn hooks_refuse_a_missing_script_or_a_taken_name() {
        let mut scripts = vec![script("saveGame", ""), script("loadGame", "")];
        assert!(hook_scripts(&mut scripts, vec![hook("nope", "a_orig")]).is_err());
        assert!(hook_scripts(&mut scripts, vec![hook("saveGame", "loadGame")]).is_err());
    }

    /// A token naming the statement `text` inside `source`, the way the mapper would write one.
    fn at(source: &str, text: &str) -> String {
        let start = source.find(text).expect("statement is in the source");
        format!("saveGame#s,{},{}", start, start + text.len())
    }

    fn insert(at: String, place: &str, call: &str) -> ModInsert {
        ModInsert { at, place: place.into(), call: call.into() }
    }

    /// Splicing one source and reading the result back.
    fn spliced(source: &str, inserts: Vec<ModInsert>) -> String {
        let mut scripts = vec![script("saveGame", source)];
        insert_into(&mut scripts, &mut Vec::new(), &mut Vec::new(), inserts).unwrap();
        body(&scripts, "saveGame")
    }

    #[test]
    fn an_insertion_runs_after_or_before_its_statement() {
        let source = "a = 1;\nb = 2;";
        assert_eq!(spliced(source, vec![insert(at(source, "a = 1;"), "after", "m_ins")]), "{a = 1;;m_ins();}\nb = 2;");
        assert_eq!(spliced(source, vec![insert(at(source, "b = 2;"), "before", "m_ins")]), "a = 1;\n{m_ins();b = 2;}");
    }

    /// The braces are what keep a braceless branch a branch. Without them this would read as
    /// `if (x) a = 1; m_ins(); else ...`, which does not compile.
    #[test]
    fn an_insertion_into_a_braceless_branch_keeps_the_branch() {
        let source = "if (x) a = 1; else b = 2;";
        assert_eq!(
            spliced(source, vec![insert(at(source, "a = 1;"), "after", "m_ins")]),
            "if (x) {a = 1;;m_ins();} else b = 2;",
        );
    }

    /// Load order, at one statement. Each wrapping the last would reverse the "before" ones.
    #[test]
    fn several_insertions_at_one_statement_keep_their_order() {
        let source = "a = 1;";
        assert_eq!(
            spliced(source, vec![
                insert(at(source, "a = 1;"), "before", "first"),
                insert(at(source, "a = 1;"), "after", "third"),
                insert(at(source, "a = 1;"), "before", "second"),
                insert(at(source, "a = 1;"), "after", "fourth"),
            ],),
            "{first();second();a = 1;;third();fourth();}",
        );
    }

    /// The case the offsets have to survive: an insertion on an `if` and one on a statement inside
    /// it. The inner edit is made first and makes the source longer, so the outer statement's
    /// recorded end no longer points where it did -- and slicing there would cut the script apart.
    #[test]
    fn insertions_nested_inside_one_another_both_land() {
        let source = "if (x) { a = 1; }";
        assert_eq!(
            spliced(source, vec![
                insert(at(source, "if (x) { a = 1; }"), "after", "outer"),
                insert(at(source, "a = 1;"), "after", "inner"),
            ],),
            "{if (x) { {a = 1;;inner();} };outer();}",
        );
    }

    /// Removing something a game does, which is the whole point of `"instead"`: the statement is
    /// not copied over, and the braces keep the place it had.
    #[test]
    fn an_insertion_instead_of_a_statement_drops_it() {
        let source = "sound_stop_all();\nsound_play(sndOnDeath);";
        assert_eq!(
            spliced(source, vec![insert(at(source, "sound_stop_all();"), "instead", "m_skip")]),
            "{m_skip();}\nsound_play(sndOnDeath);",
        );
    }

    /// A braceless branch is where dropping a statement could stop the script compiling, and does
    /// not, for the same reason an insertion around one does not.
    #[test]
    fn an_insertion_instead_of_a_braceless_branch_keeps_the_branch() {
        let source = "if (x) a = 1; else b = 2;";
        assert_eq!(
            spliced(source, vec![insert(at(source, "a = 1;"), "instead", "m_skip")]),
            "if (x) {m_skip();} else b = 2;",
        );
    }

    /// `"instead"` and the other two compose: what asked to run before the statement still runs
    /// before what replaced it.
    #[test]
    fn instead_before_and_after_all_land_at_one_statement() {
        let source = "a = 1;";
        assert_eq!(
            spliced(source, vec![
                insert(at(source, "a = 1;"), "before", "first"),
                insert(at(source, "a = 1;"), "instead", "second"),
                insert(at(source, "a = 1;"), "after", "third"),
            ]),
            "{first();second();;third();}",
        );
    }

    /// Dropping a statement makes the source *shorter*, which an outer statement's recorded end
    /// has to survive just as it survives an inner one making it longer.
    #[test]
    fn a_dropped_statement_leaves_an_outer_insertion_where_it_was() {
        let source = "if (x) { sound_stop_all(); }";
        assert_eq!(
            spliced(source, vec![
                insert(at(source, "if (x) { sound_stop_all(); }"), "after", "outer"),
                insert(at(source, "sound_stop_all();"), "instead", "m_skip"),
            ]),
            "{if (x) { {m_skip();} };outer();}",
        );
    }

    /// An event's code lives in one of its actions' buffers, and that is the one an insertion has
    /// to edit -- the retry key of an I Wanna game is a Key Press event, and that is where these
    /// games stop their music.
    #[test]
    fn an_insertion_into_an_object_event_edits_that_action() {
        let mut objects = vec![Some(Box::new(Object {
            name: PascalString::from("world"),
            sprite_index: -1,
            solid: false,
            visible: true,
            depth: 0,
            persistent: false,
            parent_index: -1,
            mask_index: -1,
            events: {
                let mut events: Vec<Vec<(u32, Vec<CodeAction>)>> = (0..EVENT_LIST_COUNT).map(|_| Vec::new()).collect();
                events[9] = vec![(82, vec![code_action("a = 1;"), code_action("sound_stop_all();")])];
                events
            },
        }))];

        // Object 0, event list 9 (Key Press), sub-event 82 ("R"), its second action, all of it.
        insert_into(&mut Vec::new(), &mut objects, &mut Vec::new(), vec![insert(
            "world.KeyPress_82#e,0,9,82,1,0,17".into(),
            "instead",
            "m_skip",
        )])
        .unwrap();

        let actions = &objects[0].as_ref().unwrap().events[9][0].1;
        assert_eq!(String::from_utf8_lossy(&actions[0].param_strings[0].0), "a = 1;");
        assert_eq!(String::from_utf8_lossy(&actions[1].param_strings[0].0), "{m_skip();}");
    }

    /// Where a GameMaker 8 game arms its room music, and the one kind of code that is neither a
    /// script nor an event: the creation code of one instance placed in one room.
    #[test]
    fn an_insertion_into_room_code_edits_that_rooms_own_buffer() {
        let room = |creation: &str, instances: Vec<&str>| {
            Some(Box::new(room::Room {
                name: PascalString::from("ringo"),
                caption: PascalString::default(),
                width: 800,
                height: 608,
                speed: 50,
                persistent: false,
                bg_colour: 0u32.into(),
                clear_screen: true,
                clear_region: true,
                creation_code: PascalString::from(creation),
                backgrounds: Vec::new(),
                views_enabled: false,
                views: Vec::new(),
                instances: instances
                    .into_iter()
                    .map(|code| room::Instance {
                        x: 0,
                        y: 0,
                        object: 0,
                        id: 1,
                        creation_code: PascalString::from(code),
                        xscale: 1.0,
                        yscale: 1.0,
                        blend: u32::MAX,
                        angle: 0.0,
                    })
                    .collect(),
                tiles: Vec::new(),
                uses_810_features: false,
                uses_811_features: false,
            }))
        };
        let mut rooms = vec![room("global.area = 2;", vec!["x = 1;", "sound_loop(snd_ringo);"])];

        insert_into(&mut Vec::new(), &mut Vec::new(), &mut rooms, vec![
            // Room 0's own creation code.
            insert("ringo.room#r,0,0,16".into(), "after", "m_area"),
            // Room 0, its *second* instance, all of it -- the one that arms the music.
            insert("ringo.instance_1#i,0,1,0,22".into(), "instead", "m_guard"),
        ])
        .unwrap();

        let room = rooms[0].as_ref().unwrap();
        let text = |s: &PascalString| String::from_utf8_lossy(&s.0).into_owned();
        assert_eq!(text(&room.creation_code), "{global.area = 2;;m_area();}");
        // The other instance in the same room is untouched, which is the whole point of routing by
        // position: one buffer per placement.
        assert_eq!(text(&room.instances[0].creation_code), "x = 1;");
        assert_eq!(text(&room.instances[1].creation_code), "{m_guard();}");
    }

    #[test]
    fn insertions_into_different_scripts_do_not_disturb_each_other() {
        let mut scripts = vec![script("saveGame", "a = 1;"), script("loadGame", "b = 2;")];
        insert_into(&mut scripts, &mut Vec::new(), &mut Vec::new(), vec![
            insert("saveGame#s,0,6".into(), "after", "one"),
            insert("loadGame#s,0,6".into(), "after", "two"),
        ])
        .unwrap();
        assert_eq!(body(&scripts, "saveGame"), "{a = 1;;one();}");
        assert_eq!(body(&scripts, "loadGame"), "{b = 2;;two();}");
    }

    /// Every refusal, because a wrong offset silently edits a game in the wrong place and the
    /// result is usually still valid GML.
    #[test]
    fn insertions_refuse_what_they_cannot_place() {
        let mut scripts = vec![script("saveGame", "a = 1;")];
        let cases = [
            ("no such script", insert("nope#0,6".into(), "after", "m")),
            ("past the end", insert("saveGame#0,999".into(), "after", "m")),
            ("backwards", insert("saveGame#6,0".into(), "after", "m")),
            ("malformed", insert("saveGame".into(), "after", "m")),
            ("not a number", insert("saveGame#a,b".into(), "after", "m")),
            ("bad place", insert("saveGame#0,6".into(), "instead", "m")),
        ];
        for (why, one) in cases {
            assert!(
                insert_into(&mut scripts, &mut Vec::new(), &mut Vec::new(), vec![one]).is_err(),
                "should have refused: {why}"
            );
        }
        // And refusing left the script alone.
        assert_eq!(body(&scripts, "saveGame"), "a = 1;");
    }

    /// GameMaker 8 source is bytes, not UTF-8, and a fangame's comments are very often Shift-JIS.
    ///
    /// This is the bug that shipped: the splice used to slice `String::from_utf8_lossy`, where each
    /// invalid byte becomes a three-byte U+FFFD. Offsets index the original bytes, so every edit in
    /// a script with one non-ASCII comment landed further and further from where it belonged --
    /// cutting statements in half and producing GML that no longer compiles. Nothing in an
    /// all-ASCII test game can catch it, which is why the bytes are spelled out here.
    #[test]
    fn an_insertion_survives_source_that_is_not_utf8() {
        // `//<93>ú–{<8C>ê` -- a Shift-JIS comment, then a statement to insert after.
        let mut source = b"//\x93\xfa\x96{\x8c\xea\r\na = 1;".to_vec();
        let start = source.len() - 6;
        let at = format!("saveGame#s,{},{}", start, source.len());
        let mut scripts = vec![Some(Box::new(Script {
            name: PascalString::from("saveGame"),
            source: PascalString(std::mem::take(&mut source).into_boxed_slice()),
        }))];

        insert_into(&mut scripts, &mut Vec::new(), &mut Vec::new(), vec![insert(at, "after", "m_ins")]).unwrap();

        let out = scripts[0].as_ref().unwrap().source.0.as_ref();
        // The comment is byte-for-byte what it was, and the statement is wrapped where it stood.
        assert_eq!(&out[..8], b"//\x93\xfa\x96{\x8c\xea");
        assert_eq!(&out[8..], b"\r\n{a = 1;;m_ins();}");
    }

    /// What the mapper writes, the installer reads: a token taken from real mapped output lands on
    /// exactly the statement it named. The two halves are written apart, so this is the seam.
    #[test]
    fn a_token_from_the_mapper_lands_where_the_mapper_meant() {
        let source = "if (argument0) { global.saveX = player.x; }";
        let token = crate::game_code::tests_support::token_for(source, "saveGame", "global.saveX = player.x;");
        let out = spliced(source, vec![insert(token, "after", "m_ins")]);
        assert_eq!(out, "if (argument0) { {global.saveX = player.x;;m_ins();} }");
    }
}
