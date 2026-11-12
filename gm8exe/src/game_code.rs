//! A GameMaker 8 game's code, summarized into an engine-neutral `GameCode` JSON tree.
//!
//! The format is shared with a GameMaker Studio producer and defined outside this crate; the two
//! are held to the same fixtures.
//!
//! **For understanding, never for rebuilding.** A mod reads this to find something in the game; it
//! never prints it back into GML. So the mapping keeps assignments, calls, references and control
//! flow, normalizes what differs between dialects, and turns everything else into `unknown` rather
//! than guessing.
//!
//! Lives beside the reader because it needs nothing but the parsed assets.

use crate::{
    GameAssets,
    asset::{CodeAction, Object, Room, Sound, sound::SoundKind},
    builtin_names::{BUILTIN_VARIABLES, CONSTANTS},
};
use gml_parser::{
    ast::{AST, Expr},
    token::Operator,
};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

/// Bumped with `GAME_CODE_FORMAT` in the protocol, never on its own.
pub const FORMAT: u32 = 4;

/// The protocol's event names in GameMaker 8's numbering, the same table as `GM8_EVENTS`.
const EVENTS: [(&str, usize, u32); 6] =
    [("create", 0, 0), ("destroy", 1, 0), ("step", 3, 0), ("beginStep", 3, 1), ("endStep", 3, 2), ("draw", 8, 0)];

/// GameMaker 8's twelve event lists, by index, named the way the IDE names them. What a code
/// entry for an event is called (`world.KeyPress_82`), so a mod's error message and a developer
/// opening the game in the IDE are talking about the same event.
const EVENT_LISTS: [&str; 12] = [
    "Create",
    "Destroy",
    "Alarm",
    "Step",
    "Collision",
    "Keyboard",
    "Mouse",
    "Other",
    "Draw",
    "KeyPress",
    "KeyRelease",
    "Trigger",
];

/// GameMaker 8's drag-and-drop action kinds and execution types, as `gm8emulator::action` reads them.
mod action {
    pub const NORMAL: u32 = 0;
    pub const EXIT: u32 = 4;
    pub const VARIABLE: u32 = 6;
    pub const CODE: u32 = 7;
    pub const EXECUTE_FUNCTION: u32 = 1;
    pub const EXECUTE_CODE: u32 = 2;
    /// `applies_to` for the instance running the action.
    pub const SELF: i32 = -1;
    /// `applies_to` for the other instance in a collision.
    pub const OTHER: i32 = -2;
}

/// Everything name resolution needs to know about the game, gathered once.
struct Names {
    scripts: HashSet<Vec<u8>>,
    /// Asset name to its kind. Scripts are in here too, so a script used as a value is an asset.
    assets: HashMap<Vec<u8>, &'static str>,
    /// The engine's constants and the game's own (Global Game Settings -> Constants).
    constants: HashSet<Vec<u8>>,
    builtins: HashSet<&'static [u8]>,
    /// Every name any code in the game declares with `globalvar`. GameMaker 8 makes such a name
    /// global for everything that runs after the declaration, which in practice means everywhere.
    globalvars: HashSet<Vec<u8>>,
    /// Object index to name, for drag-and-drop actions that name their target by number.
    objects: Vec<Option<Vec<u8>>>,
    /// Script index to name, for the "execute script" action.
    script_indices: Vec<Option<Vec<u8>>>,
}

impl Names {
    fn new(assets: &GameAssets) -> Self {
        let mut names = Names {
            scripts: HashSet::new(),
            assets: HashMap::new(),
            constants: CONSTANTS.iter().map(|c| c.as_bytes().to_vec()).collect(),
            builtins: BUILTIN_VARIABLES.iter().map(|b| b.as_bytes()).collect(),
            globalvars: HashSet::new(),
            objects: assets.objects.iter().map(|o| o.as_ref().map(|o| o.name.0.to_vec())).collect(),
            script_indices: assets.scripts.iter().map(|s| s.as_ref().map(|s| s.name.0.to_vec())).collect(),
        };
        for constant in &assets.constants {
            names.constants.insert(constant.name.0.to_vec());
        }
        macro_rules! register {
            ($list: expr, $kind: expr) => {
                for asset in $list.iter().flatten() {
                    names.assets.insert(asset.name.0.to_vec(), $kind);
                }
            };
        }
        register!(assets.sprites, "sprite");
        register!(assets.sounds, "sound");
        register!(assets.backgrounds, "other");
        register!(assets.paths, "other");
        register!(assets.fonts, "other");
        register!(assets.timelines, "other");
        register!(assets.rooms, "room");
        register!(assets.objects, "object");
        register!(assets.scripts, "script");
        for script in assets.scripts.iter().flatten() {
            names.scripts.insert(script.name.0.to_vec());
        }
        names
    }
}

/// Maps one piece of code, carrying the `var` declarations seen so far.
struct Mapper<'n> {
    names: &'n Names,
    locals: HashSet<Vec<u8>>,
    /// The code entry this is mapping, when its statements can be addressed: the name that goes in
    /// front of every `at` token, and what the loader validates one against.
    ///
    /// `None` for a piece of code with no source buffer of its own to splice: a drag-and-drop
    /// action this format spells out as GML it assembled (`action::VARIABLE`) has offsets into a
    /// string that exists nowhere in the game, so emitting tokens for it would be promising
    /// something the installer cannot keep.
    entry: Option<String>,
    /// What the installer needs to find this entry's source again, in front of the offsets:
    /// `"s"` for a script, `"e,<object>,<list>,<sub>,<action>"` for one action of one event.
    /// Read by `mods::parse_at` and by nobody else.
    route: String,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn unknown() -> Value {
    json!({ "k": "unknown" })
}

impl<'n> Mapper<'n> {
    /// A mapper that addresses nothing, which is what a test that only reads the shape wants.
    #[cfg(test)]
    fn new(names: &'n Names) -> Self {
        Mapper { names, locals: HashSet::new(), entry: None, route: String::from("s") }
    }

    /// A mapper for one of the game's scripts, whose statements a mod can point at.
    fn for_script(names: &'n Names, name: &str) -> Self {
        Mapper { names, locals: HashSet::new(), entry: Some(name.to_string()), route: String::from("s") }
    }

    /// A mapper for one of an object's events. Which action within it the statements came from is
    /// part of the route, not the name: one event's actions each have their own source buffer, and
    /// the offsets in a token only mean anything together with the buffer they came from.
    fn for_event(names: &'n Names, name: &str) -> Self {
        Mapper { names, locals: HashSet::new(), entry: Some(name.to_string()), route: String::new() }
    }

    /// A mapper for one of the pieces of code a room runs when it loads, whose route the caller
    /// already knows: `"r,<room>"` for the room's own creation code, `"i,<room>,<instance>"` for an
    /// instance's. By index, because that is what the installer can look the buffer up by -- a
    /// room's instances have no names, and two rooms may hold the same instance id.
    fn for_room_code(names: &'n Names, name: &str, route: String) -> Self {
        Mapper { names, locals: HashSet::new(), entry: Some(name.to_string()), route }
    }

    /// One statement's token: the entry it is in, then where it is in that entry's source.
    ///
    /// The two offsets are read back by this same crate when it splices (`mods::insert_into`), and
    /// by nobody else -- the loader only ever looks at the half before the `#`.
    fn at(&self, start: usize, end: usize) -> Option<String> {
        self.entry.as_ref().map(|entry| format!("{}#{},{},{}", entry, self.route, start, end))
    }

    /// A whole piece of GML. Code that does not parse is one `unknown`: GameMaker 8 only compiles
    /// what runs, so a game can ship a script that never parsed and still work.
    fn source(&mut self, source: &[u8]) -> Vec<Value> {
        // Spans only where they can be used. Parsing with them costs an allocation per statement,
        // and an event's would name an entry the installer cannot find.
        let parsed = if self.entry.is_some() { AST::new_with_spans(source) } else { AST::new(source) };
        match parsed {
            Ok(ast) => ast.iter().flat_map(|line| self.statement(line)).collect(),
            Err(_) => vec![unknown()],
        }
    }

    /// One statement. A block (`{ ... }`) flattens into its statements, so this returns several.
    fn statement(&mut self, expr: &Expr) -> Vec<Value> {
        match expr {
            // A statement with its span.
            //
            // Not for a `{}` block: it flattens into the statements inside it, which carry their
            // own tokens, and there is no single node left for its own span to sit on. A block
            // holding exactly one statement is the trap here -- it does flatten to one value, and
            // attaching would quietly give that statement the *block's* span, so an insertion
            // "after" `{ x = 1; }` would land after the closing brace instead of after `x = 1;`.
            // Outside the `if` it was meant to be inside, and still perfectly valid GML.
            //
            // Nor for a `for`, which maps to its initialiser followed by the loop: two statements,
            // so the same rule covers it.
            Expr::At(at) => {
                let mut mapped = self.statement(&at.stmt);
                if !matches!(at.stmt, Expr::Group(_)) {
                    if let ([value], Some(token)) = (mapped.as_mut_slice(), self.at(at.start, at.end)) {
                        if let Value::Object(fields) = value {
                            fields.insert("at".to_string(), Value::String(token));
                        }
                    }
                }
                mapped
            },
            Expr::Group(lines) => lines.iter().flat_map(|line| self.statement(line)).collect(),
            Expr::Binary(binary) => {
                let op = match binary.op {
                    Operator::Assign => "=",
                    Operator::AssignAdd => "+=",
                    Operator::AssignSubtract => "-=",
                    Operator::AssignMultiply => "*=",
                    Operator::AssignDivide => "/=",
                    Operator::AssignBitwiseAnd | Operator::AssignBitwiseOr | Operator::AssignBitwiseXor => "other",
                    _ => return vec![unknown()],
                };
                vec![json!({
                    "k": "assign",
                    "target": self.expression(&binary.left),
                    "op": op,
                    "value": self.expression(&binary.right),
                })]
            },
            Expr::Function(_) => match self.expression(expr) {
                call @ Value::Object(_) if call["k"] == "call" => vec![json!({ "k": "call", "call": call })],
                _ => vec![unknown()],
            },
            Expr::If(if_expr) => vec![json!({
                "k": "if",
                "cond": self.expression(&if_expr.cond),
                "then": self.statement(&if_expr.body),
                "else": if_expr.else_body.as_ref().map(|e| self.statement(e)).unwrap_or_default(),
            })],
            Expr::With(with) => vec![json!({
                "k": "with",
                "target": self.expression(&with.target),
                "body": self.statement(&with.body),
            })],
            Expr::While(w) => vec![json!({ "k": "loop", "body": self.statement(&w.body) })],
            Expr::DoUntil(d) => vec![json!({ "k": "loop", "body": self.statement(&d.body) })],
            Expr::Repeat(r) => vec![json!({ "k": "loop", "body": self.statement(&r.body) })],
            Expr::For(f) => {
                // The initialiser runs once, before the loop; the step runs inside it.
                let mut out = self.statement(&f.start);
                let mut body = self.statement(&f.body);
                body.extend(self.statement(&f.step));
                out.push(json!({ "k": "loop", "body": body }));
                out
            },
            Expr::Switch(switch) => {
                let mut bodies: Vec<Vec<Value>> = Vec::new();
                let lines: &[Expr] = match &switch.body {
                    Expr::Group(lines) => lines,
                    other => std::slice::from_ref(other),
                };
                for line in lines {
                    match line {
                        Expr::Case(_) | Expr::Default => bodies.push(Vec::new()),
                        other => {
                            let mapped = self.statement(other);
                            match bodies.last_mut() {
                                Some(body) => body.extend(mapped),
                                None => bodies.push(mapped),
                            }
                        },
                    }
                }
                vec![json!({ "k": "switch", "value": self.expression(&switch.input), "bodies": bodies })]
            },
            Expr::Var(var) => {
                for name in &var.vars {
                    self.locals.insert(name.to_vec());
                }
                Vec::new()
            },
            // Already folded into `Names::globalvars` by the pre-pass.
            Expr::GlobalVar(_) => Vec::new(),
            Expr::Exit => vec![json!({ "k": "exit", "value": Value::Null })],
            Expr::Return(value) => vec![json!({ "k": "exit", "value": self.expression(value) })],
            _ => vec![unknown()],
        }
    }

    fn expression(&mut self, expr: &Expr) -> Value {
        match expr {
            Expr::LiteralReal(value) => json!({ "k": "number", "value": value }),
            Expr::LiteralString(value) => json!({ "k": "string", "value": text(value) }),
            Expr::LiteralIdentifier(name) => self.identifier(name),
            Expr::Unary(unary) => {
                let op = match unary.op {
                    Operator::Subtract => "-",
                    Operator::Add => "+",
                    Operator::Not => "!",
                    Operator::Complement => "~",
                    _ => return unknown(),
                };
                json!({ "k": "op", "op": op, "args": [self.expression(&unary.child)] })
            },
            Expr::Binary(binary) => match binary.op {
                Operator::Deref => self.deref(&binary.left, &binary.right),
                Operator::Index => self.index(&binary.left, &binary.right),
                op => {
                    let name = match op {
                        Operator::Add => "+",
                        Operator::Subtract => "-",
                        Operator::Multiply => "*",
                        Operator::Divide => "/",
                        Operator::IntDivide => "div",
                        Operator::Modulo => "mod",
                        Operator::BitwiseAnd => "&",
                        Operator::BitwiseOr => "|",
                        Operator::BitwiseXor => "^",
                        Operator::BinaryShiftLeft => "<<",
                        Operator::BinaryShiftRight => ">>",
                        // GameMaker 8 reads `=` inside an expression as a comparison.
                        Operator::Assign | Operator::Equal => "==",
                        Operator::NotEqual => "!=",
                        Operator::LessThan => "<",
                        Operator::LessThanOrEqual => "<=",
                        Operator::GreaterThan => ">",
                        Operator::GreaterThanOrEqual => ">=",
                        Operator::And => "&&",
                        Operator::Or => "||",
                        Operator::Xor => "^^",
                        _ => return unknown(),
                    };
                    json!({ "k": "op", "op": name, "args": [self.expression(&binary.left), self.expression(&binary.right)] })
                },
            },
            Expr::Function(function) => {
                let kind = if self.names.scripts.contains(function.name) { "script" } else { "builtin" };
                let args: Vec<Value> = function.params.iter().map(|p| self.expression(p)).collect();
                json!({ "k": "call", "kind": kind, "name": text(function.name), "args": args })
            },
            // Parentheses around a single expression.
            Expr::Group(inner) if inner.len() == 1 => self.expression(&inner[0]),
            _ => unknown(),
        }
    }

    fn identifier(&self, name: &[u8]) -> Value {
        if self.locals.contains(name) {
            return json!({ "k": "var", "scope": "local", "name": text(name) })
        }
        if let Some(index) = argument_index(name) {
            return json!({ "k": "arg", "index": index })
        }
        if self.names.globalvars.contains(name) {
            return json!({ "k": "var", "scope": "global", "name": text(name) })
        }
        if let Some(kind) = self.names.assets.get(name) {
            return json!({ "k": "asset", "kind": kind, "name": text(name) })
        }
        if self.names.constants.contains(name) {
            return json!({ "k": "constant", "name": text(name) })
        }
        let scope = if self.names.builtins.contains(name) { "builtin" } else { "self" };
        json!({ "k": "var", "scope": scope, "name": text(name) })
    }

    /// `left.right`. `global.x` is a global, `self.x` is just `x`; anything else is a field of
    /// whatever `left` evaluates to.
    fn deref(&mut self, left: &Expr, right: &Expr) -> Value {
        let Expr::LiteralIdentifier(field) = right else { return unknown() };
        match left {
            Expr::LiteralIdentifier(b"global") => json!({ "k": "var", "scope": "global", "name": text(field) }),
            Expr::LiteralIdentifier(b"self") => {
                let scope = if self.names.builtins.contains(*field) { "builtin" } else { "self" };
                json!({ "k": "var", "scope": scope, "name": text(field) })
            },
            _ => json!({ "k": "field", "of": self.expression(left), "name": text(field) }),
        }
    }

    /// `left[dims]`. `argument[N]` is an argument like `argumentN` is.
    fn index(&mut self, left: &Expr, right: &Expr) -> Value {
        let dims: &[Expr] = match right {
            Expr::Group(dims) => dims,
            other => std::slice::from_ref(other),
        };
        if let Expr::LiteralIdentifier(b"argument") = left {
            let index = match dims {
                [Expr::LiteralReal(n)] if n.fract() == 0.0 && *n >= 0.0 => json!(*n as u32),
                _ => Value::Null,
            };
            return json!({ "k": "arg", "index": index })
        }
        let at: Vec<Value> = dims.iter().map(|d| self.expression(d)).collect();
        json!({ "k": "index", "of": self.expression(left), "at": at })
    }

    /// One drag-and-drop event, as the linear list of what its actions do.
    ///
    /// Conditions, blocks and `else` only change *whether* an action runs, which this format does
    /// not describe for actions anyway, so they are dropped rather than invented into structure.
    ///
    /// `event` is where these actions are -- the object, event list and sub-event -- for the tokens
    /// of the ones that have a source buffer to point into. `None` maps the same actions without
    /// addressing anything, which is what the mapper's own tests do.
    fn actions(&mut self, actions: &[CodeAction], event: Option<(usize, usize, u32)>) -> Vec<Value> {
        let mut out = Vec::new();
        let entry = self.entry.clone();
        for (index, a) in actions.iter().enumerate() {
            // Each action is its own buffer, so each needs its own route. Restored every time
            // round because the arms below drop the entry for an action that has no buffer.
            self.entry = entry.clone();
            if let Some((object, list, sub)) = event {
                self.route = format!("e,{object},{list},{sub},{index}");
            }
            let body = match a.action_kind {
                action::CODE => self.source(&a.param_strings[0].0),
                action::VARIABLE => {
                    let mut code = a.param_strings[0].0.to_vec();
                    code.extend_from_slice(if a.is_relative { b"+=" } else { b"=" });
                    code.extend_from_slice(&a.param_strings[1].0);
                    // GML this format assembled: its offsets index a string the game does not
                    // contain, so nothing here can be addressed.
                    self.entry = None;
                    self.source(&code)
                },
                action::EXIT => vec![json!({ "k": "exit", "value": Value::Null })],
                action::NORMAL if a.execution_type == action::EXECUTE_CODE => self.source(&a.fn_code.0),
                action::NORMAL if a.execution_type == action::EXECUTE_FUNCTION => vec![self.action_call(a)],
                _ => Vec::new(),
            };
            if body.is_empty() {
                continue
            }
            match a.applies_to {
                action::SELF => out.extend(body),
                target if a.applies_to_something => {
                    let target = match target {
                        action::OTHER => json!({ "k": "constant", "name": "other" }),
                        index => match self.names.objects.get(index as usize).and_then(|o| o.as_ref()) {
                            Some(name) => json!({ "k": "asset", "kind": "object", "name": text(name) }),
                            None => json!({ "k": "number", "value": index }),
                        },
                    };
                    out.push(json!({ "k": "with", "target": target, "body": body }));
                },
                _ => out.extend(body),
            }
        }
        self.entry = entry;
        out
    }

    /// A function-backed action as a call. "Execute script" names its script by index, and becomes a
    /// call to that script so a mod following calls follows it too.
    fn action_call(&mut self, a: &CodeAction) -> Value {
        let params: Vec<&[u8]> = a.param_strings.iter().take(a.param_count).map(|p| p.0.as_ref()).collect();
        let parse = |mapper: &mut Self, source: &[u8]| match AST::expression(source) {
            Ok(expr) => mapper.expression(&expr),
            Err(_) => json!({ "k": "string", "value": text(source) }),
        };
        if a.fn_name.0.as_ref() == b"action_execute_script" {
            let script = params.first().and_then(|p| {
                let index: usize = std::str::from_utf8(p).ok()?.trim().parse().ok()?;
                self.names.script_indices.get(index)?.clone()
            });
            if let Some(name) = script {
                let args: Vec<Value> = params.iter().skip(1).map(|p| parse(self, p)).collect();
                return json!({ "k": "call", "call": { "k": "call", "kind": "script", "name": text(&name), "args": args } })
            }
        }
        let args: Vec<Value> = params.iter().map(|p| parse(self, p)).collect();
        json!({ "k": "call", "call": { "k": "call", "kind": "builtin", "name": text(&a.fn_name.0), "args": args } })
    }
}

/// `argument0` .. `argument15`, as their number.
fn argument_index(name: &[u8]) -> Option<u32> {
    let digits = name.strip_prefix(b"argument")?;
    if digits.is_empty() || digits.len() > 2 || !digits.iter().all(u8::is_ascii_digit) {
        return None
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

/// Collects every `globalvar` declaration in a piece of code, recursively.
fn collect_globalvars(expr: &Expr, into: &mut HashSet<Vec<u8>>) {
    match expr {
        Expr::GlobalVar(var) => into.extend(var.vars.iter().map(|v| v.to_vec())),
        Expr::Group(lines) => lines.iter().for_each(|e| collect_globalvars(e, into)),
        Expr::If(e) => {
            collect_globalvars(&e.body, into);
            if let Some(els) = &e.else_body {
                collect_globalvars(els, into);
            }
        },
        Expr::With(e) => collect_globalvars(&e.body, into),
        Expr::While(e) => collect_globalvars(&e.body, into),
        Expr::DoUntil(e) => collect_globalvars(&e.body, into),
        Expr::Repeat(e) => collect_globalvars(&e.body, into),
        Expr::For(e) => collect_globalvars(&e.body, into),
        Expr::Switch(e) => collect_globalvars(&e.body, into),
        _ => (),
    }
}

/// Every piece of source code in the game that the summary covers: scripts, the code actions of
/// every event, and the creation code of every room and of every instance placed in one. Both of an action's buffers, since which one holds its GML depends on the kind
/// of action it is (see `Mapper::actions`) and a `globalvar` anywhere makes a name global
/// everywhere.
fn all_sources(assets: &GameAssets) -> Vec<&[u8]> {
    let mut sources: Vec<&[u8]> = assets.scripts.iter().flatten().map(|s| s.source.0.as_ref()).collect();
    for object in assets.objects.iter().flatten() {
        for subs in &object.events {
            for (_, actions) in subs {
                for a in actions {
                    sources.push(&a.param_strings[0].0);
                    sources.push(&a.fn_code.0);
                }
            }
        }
    }
    for room in assets.rooms.iter().flatten() {
        sources.push(&room.creation_code.0);
        for instance in &room.instances {
            sources.push(&instance.creation_code.0);
        }
    }
    sources
}

/// The objects, and every event of every object as its own code entry.
///
/// Every event is mapped exactly once. The six the protocol names appear in the object's own
/// `events` as well, as the same value -- two views of one piece of code, never two mappings of it
/// that could disagree.
fn objects_and_events(names: &Names, objects: &[Option<Box<Object>>]) -> (Vec<Value>, Vec<Value>) {
    let mut entries: Vec<Value> = Vec::new();
    let mapped: Vec<Value> = objects
        .iter()
        .enumerate()
        .filter_map(|(index, object)| object.as_ref().map(|object| (index, object)))
        .map(|(index, object)| {
            let object_name = text(&object.name.0);
            let mut events = Map::new();
            for (list, subs) in object.events.iter().enumerate() {
                for (sub, actions) in subs {
                    let name = format!("{}.{}_{}", object_name, EVENT_LISTS.get(list).copied().unwrap_or("Event"), sub);
                    let body =
                        Value::Array(Mapper::for_event(names, &name).actions(actions, Some((index, list, *sub))));
                    if let Some((event, _, _)) = EVENTS.iter().find(|(_, l, s)| *l == list && s == sub) {
                        events.insert((*event).to_string(), body.clone());
                    }
                    entries.push(json!({ "name": name, "body": body }));
                }
            }
            json!({ "name": object_name, "events": events })
        })
        .collect();
    (mapped, entries)
}

/// Every piece of code a room runs when it loads, as its own entry.
///
/// A room's own creation code, then the creation code of each instance placed in it. Empty buffers
/// are skipped: nearly every instance in a real game has none, and an entry whose body is an empty
/// block would tell a mod nothing while making this list the largest thing in the summary.
///
/// An instance is named by the id the room editor gave it rather than by its position in the room,
/// because the id is what a developer sees in the IDE. The *position* is what goes in the route, as
/// the only thing the installer can find the buffer by -- two rooms can hold instances with the
/// same id, since a room copied in the IDE copies them.
fn room_entries(names: &Names, rooms: &[Option<Box<Room>>]) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for (index, room) in rooms.iter().enumerate().filter_map(|(i, r)| r.as_ref().map(|r| (i, r))) {
        let room_name = text(&room.name.0);
        if !room.creation_code.0.is_empty() {
            let name = format!("{room_name}.room");
            let body = Mapper::for_room_code(names, &name, format!("r,{index}")).source(&room.creation_code.0);
            entries.push(json!({ "name": name, "body": body, "room": room_name, "object": Value::Null }));
        }
        for (slot, instance) in room.instances.iter().enumerate() {
            if instance.creation_code.0.is_empty() {
                continue
            }
            let name = format!("{}.instance_{}", room_name, instance.id);
            let body =
                Mapper::for_room_code(names, &name, format!("i,{index},{slot}")).source(&instance.creation_code.0);
            let object = usize::try_from(instance.object)
                .ok()
                .and_then(|index| names.objects.get(index))
                .and_then(|name| name.as_ref())
                .map_or(Value::Null, |name| Value::String(text(name)));
            entries.push(json!({ "name": name, "body": body, "room": room_name, "object": object }));
        }
    }
    entries
}

/// The sound assets, saying which of them hold GameMaker 8's one multimedia slot.
///
/// What the code cannot say. Every mp3 holds it, whatever kind it claims, which is how the engine
/// itself plays one (`audio.play_mp3` is always exclusive); a wav holds it only when the game
/// imported it as multimedia.
fn sound_entries(sounds: &[Option<Box<Sound>>]) -> Vec<Value> {
    sounds
        .iter()
        .flatten()
        .map(|sound| {
            let exclusive =
                sound.extension.0.as_ref().eq_ignore_ascii_case(b".mp3") || sound.kind == SoundKind::Multimedia;
            json!({ "name": text(&sound.name.0), "exclusive": exclusive })
        })
        .collect()
}

/// The game's code in the protocol's `GameCode` shape, as a JSON value.
pub fn game_code(assets: &GameAssets) -> Value {
    let mut names = Names::new(assets);
    let mut globalvars = HashSet::new();
    for source in all_sources(assets) {
        if let Ok(ast) = AST::new(source) {
            ast.iter().for_each(|line| collect_globalvars(line, &mut globalvars));
        }
    }
    names.globalvars = globalvars;

    let scripts: Vec<Value> = assets
        .scripts
        .iter()
        .flatten()
        .map(|script| {
            let name = text(&script.name.0);
            let body = Mapper::for_script(&names, &name).source(&script.source.0);
            json!({ "name": name, "body": body })
        })
        .collect();

    let (objects, entries) = objects_and_events(&names, &assets.objects);
    let sounds = sound_entries(&assets.sounds);
    let rooms = room_entries(&names, &assets.rooms);

    json!({
        "format": FORMAT,
        "scripts": scripts,
        "objects": objects,
        "events": entries,
        "sounds": sounds,
        "rooms": rooms,
    })
}

/// `game_code`, serialized. What the wasm exports hand to JavaScript.
pub fn game_code_json(assets: &GameAssets) -> String {
    game_code(assets).to_string()
}

/// Real mapper output, for tests in other modules of this crate.
///
/// The installer's tests use it so that at least one of them splices a token this mapper actually
/// wrote, rather than one hand-computed to match. The two halves are written independently, and
/// that seam is where an off-by-one would edit a game in the wrong place and still compile.
#[cfg(test)]
pub mod tests_support {
    use super::*;

    /// A token's offsets, whatever route it carries: they are the last two fields of the payload.
    pub fn offsets_of(token: &str) -> (usize, usize) {
        let (_, payload) = token.split_once('#').expect("token is <entry>#<route>,<start>,<end>");
        let fields: Vec<&str> = payload.split(',').collect();
        let number = |field: &str| field.parse::<usize>().expect("an offset is a number");
        match fields.as_slice() {
            [.., start, end] => (number(start), number(end)),
            _ => panic!("token {token:?} carries no offsets"),
        }
    }

    /// The token of the statement whose source text is exactly `statement`.
    pub fn token_for(source: &str, script: &str, statement: &str) -> String {
        fn walk(value: &Value, source: &str, statement: &str, found: &mut Option<String>) {
            if let Some(items) = value.as_array() {
                items.iter().for_each(|item| walk(item, source, statement, found));
                return;
            }
            let Some(fields) = value.as_object() else { return };
            if let Some(Value::String(token)) = fields.get("at") {
                let (start, end) = offsets_of(token);
                if &source[start..end] == statement {
                    *found = Some(token.clone());
                }
            }
            for key in ["then", "else", "body", "bodies"] {
                if let Some(nested) = fields.get(key) {
                    walk(nested, source, statement, found);
                }
            }
        }
        let names = tests::names();
        let body = Value::Array(Mapper::for_script(&names, script).source(source.as_bytes()));
        let mut found = None;
        walk(&body, source, statement, &mut found);
        found.unwrap_or_else(|| panic!("no statement in {source:?} has the text {statement:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn names() -> Names {
        let mut names = Names {
            scripts: HashSet::new(),
            assets: HashMap::new(),
            constants: CONSTANTS.iter().map(|c| c.as_bytes().to_vec()).collect(),
            builtins: BUILTIN_VARIABLES.iter().map(|b| b.as_bytes()).collect(),
            globalvars: HashSet::new(),
            objects: Vec::new(),
            script_indices: Vec::new(),
        };
        names.scripts.insert(b"saveGame".to_vec());
        names.assets.insert(b"saveGame".to_vec(), "script");
        names.assets.insert(b"player".to_vec(), "object");
        names.globalvars.insert(b"legacy".to_vec());
        names
    }

    fn map(source: &str) -> Value {
        let names = names();
        Value::Array(Mapper::new(&names).source(source.as_bytes()))
    }

    /// The same, as one of the game's scripts, so its statements carry `at`.
    fn map_script(source: &str) -> Value {
        let names = names();
        Value::Array(Mapper::for_script(&names, "saveGame").source(source.as_bytes()))
    }

    /// Every token in a mapped body, paired with the source text its offsets name.
    ///
    /// This is the property the whole GameMaker 8 side rests on: the installer splices by slicing
    /// the source at these offsets, so a token naming the wrong bytes edits the game in the wrong
    /// place -- and, being a text splice, most likely still compiles.
    fn tokens_with_text(source: &str) -> Vec<(String, String)> {
        fn walk(value: &Value, source: &str, out: &mut Vec<(String, String)>) {
            if let Some(body) = value.as_array() {
                body.iter().for_each(|item| walk(item, source, out));
                return;
            }
            let Some(fields) = value.as_object() else { return };
            if let Some(Value::String(token)) = fields.get("at") {
                let (entry, _) = token.split_once('#').expect("token is <entry>#<route>,<start>,<end>");
                let (start, end) = tests_support::offsets_of(token);
                out.push((entry.to_string(), source[start..end].to_string()));
            }
            for key in ["then", "else", "body", "bodies"] {
                if let Some(nested) = fields.get(key) {
                    walk(nested, source, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(&map_script(source), source, &mut out);
        out
    }

    #[test]
    fn statements_of_a_script_carry_where_they_are() {
        let source = "global.saveX = player.x;\nsaveGame(0);";
        assert_eq!(tokens_with_text(source), vec![
            ("saveGame".to_string(), "global.saveX = player.x;".to_string()),
            ("saveGame".to_string(), "saveGame(0);".to_string()),
        ],);
    }

    /// Reaching inside an `if` is the point of the whole feature: this is the statement quicksave
    /// points at, instead of inferring a guard from the `if` around it.
    #[test]
    fn statements_nested_in_control_flow_carry_it_too() {
        let source = "if (argument0) { global.saveX = player.x; }";
        assert_eq!(tokens_with_text(source).into_iter().map(|(_, text)| text).collect::<Vec<_>>(), vec![
            "if (argument0) { global.saveX = player.x; }",
            "global.saveX = player.x;"
        ],);
    }

    /// Code mapped without an entry promises no insertion, which is what a piece of code with no
    /// source buffer of its own gets.
    #[test]
    fn code_mapped_without_an_entry_carries_no_token() {
        assert_eq!(map("global.saveX = player.x;")[0].get("at"), None);
    }

    /// One object with one event, holding `actions`.
    fn object_with(name: &str, list: usize, sub: u32, actions: Vec<CodeAction>) -> Option<Box<Object>> {
        let mut events: Vec<Vec<(u32, Vec<CodeAction>)>> = (0..12).map(|_| Vec::new()).collect();
        events[list] = vec![(sub, actions)];
        Some(Box::new(Object {
            name: crate::asset::PascalString::from(name),
            sprite_index: -1,
            solid: false,
            visible: true,
            depth: 0,
            persistent: false,
            parent_index: -1,
            mask_index: -1,
            events,
        }))
    }

    fn room_with(name: &str, creation: &str, instances: Vec<(i32, i32, &str)>) -> Option<Box<Room>> {
        use crate::asset::{PascalString, room::Instance};
        Some(Box::new(Room {
            name: PascalString::from(name),
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
                .map(|(object, id, code)| Instance {
                    x: 0,
                    y: 0,
                    object,
                    id,
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
    }

    /// What a GameMaker 8 game's room music is armed by, and the reason this list exists: a line of
    /// creation code on one placed instance, which is in no script and in no object's events.
    #[test]
    fn a_rooms_own_code_and_each_instances_are_their_own_entries() {
        let mut names = names();
        names.objects = vec![Some(b"playMusic".to_vec())];
        names.assets.insert(b"snd_ringo".to_vec(), "sound");
        let rooms = vec![room_with("ringo", "global.area = 2;", vec![
            (0, 141058, "sound_loop(snd_ringo);"),
            // No creation code of its own, which is nearly every instance in a real room.
            (0, 141059, ""),
        ])];

        let entries = room_entries(&names, &rooms);
        assert_eq!(entries.len(), 2, "the empty one is skipped: {entries:?}");

        assert_eq!(entries[0]["name"], "ringo.room");
        assert_eq!(entries[0]["room"], "ringo");
        assert_eq!(entries[0]["object"], Value::Null, "the room's own code belongs to no instance");

        assert_eq!(entries[1]["name"], "ringo.instance_141058");
        assert_eq!(entries[1]["room"], "ringo");
        assert_eq!(entries[1]["object"], "playMusic");
    }

    /// The route is what the installer finds the buffer by, and an instance's is its *position* in
    /// the room rather than its id -- two rooms the IDE copied hold the same ids.
    #[test]
    fn room_code_is_routed_by_index_so_the_installer_can_find_the_buffer() {
        let mut names = names();
        names.objects = vec![Some(b"playMusic".to_vec())];
        names.assets.insert(b"snd_ringo".to_vec(), "sound");
        let rooms =
            vec![None, room_with("ringo", "global.area = 2;", vec![(0, 7, ""), (0, 141058, "sound_loop(snd_ringo);")])];

        let entries = room_entries(&names, &rooms);
        let token_of = |entry: &Value| entry["body"][0]["at"].as_str().unwrap().to_string();
        assert_eq!(token_of(&entries[0]), "ringo.room#r,1,0,16");
        // Room 1, instance 1 -- the second placement, although its id is 141058.
        assert_eq!(token_of(&entries[1]), "ringo.instance_141058#i,1,1,0,22");
    }

    /// The event a mod has to reach in an I Wanna game is the retry key, which is none of the six
    /// the protocol names -- so every event is listed, and its statements are addressable.
    #[test]
    fn an_events_statements_say_which_event_and_which_action_they_are_in() {
        let objects = vec![object_with("world", 9, 82, vec![
            crate::mods::code_action("sound_stop_all();"),
            crate::mods::code_action("loadGame();"),
        ])];

        let (_, events) = objects_and_events(&names(), &objects);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["name"], "world.KeyPress_82");
        let body = events[0]["body"].as_array().unwrap();
        // The second action's statement is routed to the second action, and its offsets are
        // measured in that action's own buffer -- both start at 0.
        assert_eq!(body[0]["at"], "world.KeyPress_82#e,0,9,82,0,0,17");
        assert_eq!(body[1]["at"], "world.KeyPress_82#e,0,9,82,1,0,11");
    }

    /// One mapping, two views: a mod reaching for the player's Create and a mod walking everything
    /// are looking at the same statements, tokens included.
    #[test]
    fn the_six_named_events_are_the_same_code_as_the_flat_list() {
        let objects = vec![object_with("player", 0, 0, vec![crate::mods::code_action("global.grav = 1;")])];

        let (mapped, events) = objects_and_events(&names(), &objects);
        assert_eq!(mapped[0]["events"]["create"], events[0]["body"]);
        assert_eq!(events[0]["name"], "player.Create_0");
        assert_eq!(mapped[0]["events"]["create"][0]["at"], "player.Create_0#e,0,0,0,0,0,16");
    }

    /// Which sounds share GameMaker 8's one multimedia slot, which no amount of reading the code
    /// could tell a mod.
    #[test]
    fn sounds_say_which_ones_hold_the_music_slot() {
        let sounds = vec![
            sound("snd_bgm", ".mp3", SoundKind::Multimedia),
            sound("sndJump", ".wav", SoundKind::Normal),
            // An mp3 is played through the multimedia player whatever kind it claims to be, which
            // is what the engine does with one too.
            sound("snd_odd", ".MP3", SoundKind::Normal),
            sound("sndStream", ".wav", SoundKind::Multimedia),
        ];

        assert_eq!(
            json!(sound_entries(&sounds)),
            json!([
                { "name": "snd_bgm", "exclusive": true },
                { "name": "sndJump", "exclusive": false },
                { "name": "snd_odd", "exclusive": true },
                { "name": "sndStream", "exclusive": true },
            ])
        );
    }

    fn sound(name: &str, extension: &str, kind: SoundKind) -> Option<Box<Sound>> {
        Some(Box::new(Sound {
            name: crate::asset::PascalString::from(name),
            source: crate::asset::PascalString::default(),
            extension: crate::asset::PascalString::from(extension),
            kind,
            volume: 1.0,
            pan: 0.0,
            preload: true,
            fx: crate::asset::sound::SoundFX {
                chorus: false,
                echo: false,
                flanger: false,
                gargle: false,
                reverb: false,
            },
            data: None,
        }))
    }

    /// The parts of a `for` header are not statements of this tree, so they get no token even
    /// though the parser reads them with the same function.
    #[test]
    fn the_parts_of_a_for_header_carry_no_token() {
        let source = "for (i = 0; i < 3; i += 1) { a = i; }";
        let texts: Vec<String> = tokens_with_text(source).into_iter().map(|(_, t)| t).collect();
        assert!(!texts.iter().any(|t| t == "i = 0" || t == "i += 1"), "got {texts:?}");
        assert!(texts.iter().any(|t| t == "a = i;"), "got {texts:?}");
    }

    #[test]
    fn assignment_and_comparison_are_told_apart() {
        assert_eq!(
            map("if (global.grav = 1) x = 2"),
            json!([{
                "k": "if",
                "cond": { "k": "op", "op": "==", "args": [
                    { "k": "var", "scope": "global", "name": "grav" },
                    { "k": "number", "value": 1.0 },
                ]},
                "then": [{ "k": "assign", "op": "=",
                    "target": { "k": "var", "scope": "builtin", "name": "x" },
                    "value": { "k": "number", "value": 2.0 } }],
                "else": [],
            }])
        );
    }

    #[test]
    fn names_resolve_to_what_they_are() {
        assert_eq!(
            map("var tem; tem = player.x; saveGame(argument0, argument[1], c_red, legacy, mine)"),
            json!([
                { "k": "assign", "op": "=",
                  "target": { "k": "var", "scope": "local", "name": "tem" },
                  "value": { "k": "field", "name": "x", "of": { "k": "asset", "kind": "object", "name": "player" } } },
                { "k": "call", "call": { "k": "call", "kind": "script", "name": "saveGame", "args": [
                    { "k": "arg", "index": 0 },
                    { "k": "arg", "index": 1 },
                    { "k": "constant", "name": "c_red" },
                    { "k": "var", "scope": "global", "name": "legacy" },
                    { "k": "var", "scope": "self", "name": "mine" },
                ]}},
            ])
        );
    }

    #[test]
    fn numeric_instance_references_stay_numbers() {
        assert_eq!(
            map("tem = (428).x"),
            json!([{ "k": "assign", "op": "=",
                "target": { "k": "var", "scope": "self", "name": "tem" },
                "value": { "k": "field", "name": "x", "of": { "k": "number", "value": 428.0 } } }])
        );
    }

    #[test]
    fn control_flow_keeps_what_runs_inside() {
        let mapped = map("for (i = 0; i < 3; i += 1) { with (player) instance_destroy(); } exit");
        assert_eq!(mapped[1]["k"], "loop");
        assert_eq!(mapped[1]["body"][0]["k"], "with");
        assert_eq!(mapped[2]["k"], "exit");
    }

    #[test]
    fn code_that_does_not_parse_is_unknown() {
        assert_eq!(map("if (("), json!([{ "k": "unknown" }]));
    }
}
