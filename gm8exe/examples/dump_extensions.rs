//! Dumps every extension package embedded in a GM8 exe: each file's kind, its declared
//! functions/constants, (for GML files) its plaintext source, and every other payload byte-for-byte
//! -- the sprites, shaders and DLLs a package unpacks into `temp_directory` at startup.
//!
//! A `.gmk` stores extensions by name only -- the `.exe` embeds them whole -- so this is the
//! only way to see the GML wrapper scripts and DLL signatures a game actually ships with.
//!
//! Usage: cargo run --example dump_extensions -p gm8exe -- <path-to-exe> <output-dir>

use gm8exe::asset::extension::FileKind;
use std::{fs, io::Write, path::PathBuf};

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let exe_path = args.next().expect("usage: dump_extensions <exe> <out-dir>");
    let out_dir = PathBuf::from(args.next().expect("usage: dump_extensions <exe> <out-dir>"));

    let mut bytes = fs::read(&exe_path).expect("failed to read exe");
    let assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, true).expect("failed to parse exe");

    fs::create_dir_all(&out_dir).expect("failed to create out dir");
    let mut summary = String::new();

    for ext in &assets.extensions {
        summary.push_str(&format!("\n=== EXTENSION: {} (folder: {}) ===\n", ext.name, ext.folder_name));
        let ext_dir = out_dir.join(sanitize(&ext.name.to_string()));
        for file in &ext.files {
            let kind = match file.kind {
                FileKind::DynamicLibrary => "dll",
                FileKind::GmlScript => "gml",
                FileKind::ActionLibrary => "lib",
                FileKind::Other => "other",
                FileKind::Unknown(_) => "unknown",
            };
            summary.push_str(&format!(
                "  [{kind}] {} -- {} functions, {} consts, {} bytes, init={:?} final={:?}\n",
                file.name,
                file.functions.len(),
                file.consts.len(),
                file.contents.len(),
                file.initializer.to_string(),
                file.finalizer.to_string(),
            ));
            for f in &file.functions {
                summary
                    .push_str(&format!("      fn {} -> extname {} ({} args)\n", f.name, f.external_name, f.arg_count));
            }
            for c in &file.consts {
                summary.push_str(&format!("      const {} = {}\n", c.name, c.value));
            }
            // Everything but the DLLs, which are large and are the one kind nothing here can read.
            // GML is plaintext; the rest -- compiled shaders, sprites, images -- is what the
            // package unpacks into `temp_directory` at startup, and is otherwise only reachable by
            // running the game.
            if file.kind != FileKind::DynamicLibrary && !file.contents.is_empty() {
                fs::create_dir_all(&ext_dir).ok();
                let mut fh = fs::File::create(ext_dir.join(sanitize(&file.name.to_string()))).unwrap();
                fh.write_all(&file.contents).unwrap();
            }
        }
    }

    fs::write(out_dir.join("_summary.txt"), &summary).expect("failed to write summary");
    println!("{summary}");
    println!("wrote {}", out_dir.display());
}
