//! Dumps all GML source (scripts + object event code actions) from a GM8 exe into a folder
//! of plain-text files, for manual/offline debugging of a game's own logic (e.g. its save/load
//! scripts). Not part of the emulator itself -- a one-off developer tool.
//!
//! Usage: cargo run --example dump_gml -p gm8exe -- <path-to-exe> <output-dir>

use gm8exe::asset::CodeAction;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const EVENT_NAMES: [&str; 12] = [
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

// Mirrors gm8emulator's src/action.rs interpretation of a CodeAction: the GML text for a
// "code" widget (action_kind 7, the common case for actual GML written in the IDE) lives in
// param_strings[0], NOT fn_code -- fn_code is "usually only provided by extensions" per its own
// doc comment. Getting this wrong makes every hand-written code action decompile as empty.
fn action_to_gml(action: &CodeAction) -> String {
    match action.action_kind {
        7 => action.param_strings[0].to_string(), // kind::CODE
        6 => {
            // kind::VARIABLE: param_strings[0] (op)= param_strings[1]
            let op = if action.is_relative { "+=" } else { "=" };
            format!("{}{}{};", action.param_strings[0], op, action.param_strings[1])
        },
        5 => format!("repeat ({}) {{", action.param_strings[0]), // kind::REPEAT
        3 => "} else {".to_string(),                             // kind::ELSE
        1 => "{".to_string(),                                    // kind::BEGIN_GROUP
        2 => "}".to_string(),                                    // kind::END_GROUP
        4 => "exit;".to_string(),                                // kind::EXIT
        0 => match action.execution_type {
            1 => format!(
                "// [built-in action] fn_name={} params=[{}]",
                action.fn_name,
                action
                    .param_strings
                    .iter()
                    .take(action.param_count)
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            2 => action.fn_code.to_string(),
            _ => String::new(), // execution_type::NONE
        },
        _ => String::new(),
    }
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("failed to create dir");
    }
    let mut f = fs::File::create(path).expect("failed to create file");
    f.write_all(contents.as_bytes()).expect("failed to write file");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let exe_path = args.next().expect("usage: dump_gml <exe> <out-dir>");
    let out_dir = args.next().expect("usage: dump_gml <exe> <out-dir>");
    let out_dir = PathBuf::from(out_dir);

    let mut bytes = fs::read(&exe_path).expect("failed to read exe");
    let assets = gm8exe::reader::from_exe(&mut bytes, Some(|s: &str| println!("{s}")), true, false)
        .expect("failed to parse exe");

    // Scripts: plain GML source, one file each.
    let scripts_dir = out_dir.join("scripts");
    let mut index_out = String::new();
    for (idx, script) in assets.scripts.iter().enumerate() {
        if let Some(script) = script {
            let name = sanitize(&script.name.to_string());
            write_file(&scripts_dir.join(format!("{name}.gml")), &script.source.to_string());
            index_out.push_str(&format!("{idx}: {}\n", script.name));
        }
    }
    write_file(&scripts_dir.join("_index.txt"), &index_out);
    println!("wrote {} scripts", assets.scripts.iter().flatten().count());

    // Objects: one file per event (sub-event), containing every code action's GML in order,
    // with non-code (drag-and-drop) actions rendered as a comment describing what they do.
    let objects_dir = out_dir.join("objects");
    let mut object_count = 0;
    let mut object_index_out = String::new();
    for (obj_idx, object) in assets.objects.iter().enumerate() {
        let Some(object) = object else { continue };
        object_count += 1;
        let parent_name = usize::try_from(object.parent_index)
            .ok()
            .and_then(|idx| assets.objects.get(idx))
            .and_then(|o| o.as_ref())
            .map(|o| o.name.to_string());
        object_index_out.push_str(&format!(
            "{obj_idx}: {} parent_index={} parent={}\n",
            object.name,
            object.parent_index,
            parent_name.as_deref().unwrap_or("<none>")
        ));
        let obj_name = sanitize(&object.name.to_string());
        for (list_idx, sub_events) in object.events.iter().enumerate() {
            let event_name = EVENT_NAMES.get(list_idx).copied().unwrap_or("Unknown");
            for (sub_id, actions) in sub_events {
                let mut out = String::new();
                out.push_str(&format!("// Object: {}\n// Event: {} (sub {})\n\n", object.name, event_name, sub_id));
                for action in actions {
                    out.push_str(&action_to_gml(action));
                    out.push('\n');
                }
                let file_name = format!("{event_name}_{sub_id}.gml");
                write_file(&objects_dir.join(&obj_name).join(&file_name), &out);
            }
        }
    }
    write_file(&objects_dir.join("_index.txt"), &object_index_out);
    println!("wrote events for {} objects", object_count);

    // Rooms: creation code (room-level) and per-instance creation code, since these are a
    // common place for "check a save/temp file on startup" logic that isn't in any object event.
    let rooms_dir = out_dir.join("rooms");
    let mut room_count = 0;
    let mut room_index_out = String::new();
    for (room_idx, room) in assets.rooms.iter().enumerate() {
        let Some(room) = room else { continue };
        room_count += 1;
        room_index_out.push_str(&format!("{room_idx}: {}\n", room.name));
        let room_name = sanitize(&room.name.to_string());
        if !room.creation_code.0.is_empty() {
            write_file(&rooms_dir.join(format!("{room_name}_creation_code.gml")), &room.creation_code.to_string());
        }
        let mut instances_out = String::new();
        for instance in &room.instances {
            let obj_name = instance
                .object
                .try_into()
                .ok()
                .and_then(|idx: usize| assets.objects.get(idx))
                .and_then(|o| o.as_ref())
                .map(|o| o.name.to_string())
                .unwrap_or_else(|| format!("<invalid:{}>", instance.object));
            instances_out
                .push_str(&format!("id={} object={} x={} y={}\n", instance.id, obj_name, instance.x, instance.y));
        }
        if !instances_out.is_empty() {
            write_file(&rooms_dir.join(format!("{room_name}_instances.txt")), &instances_out);
        }
        for (i, instance) in room.instances.iter().enumerate() {
            if !instance.creation_code.0.is_empty() {
                write_file(
                    &rooms_dir.join(&room_name).join(format!("instance_{i}_id{}_creation_code.gml", instance.id)),
                    &instance.creation_code.to_string(),
                );
            }
        }
    }
    write_file(&rooms_dir.join("_index.txt"), &room_index_out);
    println!("wrote creation code for {} rooms", room_count);
    println!("room order (first-to-last): {:?}", assets.room_order);

    // Timelines: same CodeAction moments as objects.
    let timelines_dir = out_dir.join("timelines");
    let mut timeline_count = 0;
    for timeline in assets.timelines.iter().flatten() {
        timeline_count += 1;
        let name = sanitize(&timeline.name.to_string());
        for (moment_id, actions) in &timeline.moments {
            let mut out = String::new();
            out.push_str(&format!("// Timeline: {}\n// Moment: {}\n\n", timeline.name, moment_id));
            for action in actions {
                out.push_str(&action_to_gml(action));
                out.push('\n');
            }
            write_file(&timelines_dir.join(&name).join(format!("moment_{moment_id}.gml")), &out);
        }
    }
    println!("wrote moments for {} timelines", timeline_count);

    // Triggers: single-expression conditions. Indexed by asset position (not just
    // constant_name/kind) since multiple triggers can share an empty/duplicate constant_name,
    // which would otherwise silently overwrite each other.
    let triggers_dir = out_dir.join("triggers");
    let mut trigger_count = 0;
    for (idx, trigger) in assets.triggers.iter().enumerate() {
        let Some(trigger) = trigger else { continue };
        trigger_count += 1;
        let name = sanitize(&trigger.constant_name.to_string());
        write_file(
            &triggers_dir.join(format!("{idx}_{name}.gml")),
            &format!(
                "// moment: {}\n// constant_name: {}\n\n{}",
                trigger.moment, trigger.constant_name, trigger.condition
            ),
        );
    }
    println!("wrote {} triggers", trigger_count);

    println!("done: {}", out_dir.display());
}
