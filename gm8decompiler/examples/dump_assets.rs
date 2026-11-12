//! Dumps a GM8 executable's images out as PNGs -- sprite frames by default, or the background
//! assets (which is where tilesets live) with `--backgrounds`.
//!
//! gm8decompiler itself only round-trips to .gmk, where images stay as raw zlib streams. This goes
//! straight through gm8exe's loader instead, so each one comes out as a named, viewable file.
//!
//!     cargo run --release --example dump_assets -- <game.exe> <out-dir> \
//!         [--backgrounds] [--no-swap] [--limit N]
//!
//! `--no-swap` writes the loader's bytes verbatim; by default they're treated as BGRA and swapped
//! to RGBA (GM8 stores Windows DIB order, whatever gm8exe's sprite docs claim).

use flate2::{Compression, write::ZlibEncoder};
use std::{collections::HashMap, env, fs, io::Write, path::PathBuf};

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(kind);
    body.extend_from_slice(payload);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// Minimal 8-bit RGBA PNG. Every scanline uses filter 0 -- these are small sprites, and the point
/// is a file something else can open, not the smallest possible one.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    for row in rgba.chunks_exact(width as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw)?;
    let idat = z.finish()?;

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit, truecolour+alpha
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// Asset names come from GML, so they're already close to filename-safe -- this is just a guard.
fn sanitize(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' }).collect();
    if cleaned.trim_matches('_').is_empty() { "unnamed".into() } else { cleaned }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        eprintln!("usage: dump_assets <game.exe> <out-dir> [--backgrounds] [--no-swap] [--limit N]");
        std::process::exit(2);
    }
    let swap = !args.iter().any(|a| a == "--no-swap");
    let backgrounds = args.iter().any(|a| a == "--backgrounds");
    let limit = args
        .iter()
        .position(|a| a == "--limit")
        .and_then(|i| args.get(i + 1))
        .and_then(|n| n.parse::<usize>().ok())
        .unwrap_or(usize::MAX);

    let exe_path = PathBuf::from(positional[0]);
    let out_dir = PathBuf::from(positional[1]);
    let bytes = fs::read(&exe_path).expect("failed to read exe");

    // strict=false: fangames are often built by tools that bend the format slightly, and a hard
    // failure here would lose the whole dump over one odd asset.
    let assets = gm8exe::reader::from_exe(bytes, None::<fn(&str)>, false, true).expect("failed to parse gamedata");

    fs::create_dir_all(&out_dir).expect("failed to create out dir");

    let (mut assets_written, mut images, mut empty) = (0usize, 0usize, 0usize);
    let mut used: HashMap<String, usize> = HashMap::new();

    // GM8 asset names are unique in practice; the counter is a belt-and-braces fallback so a
    // duplicate can never silently overwrite an earlier one.
    let mut unique = |base: String, index: usize| -> String {
        let seen = used.entry(base.clone()).or_insert(0);
        let name = if *seen == 0 { base } else { format!("{}_{}", base, index) };
        *seen += 1;
        name
    };

    let emit = |path: PathBuf, width: u32, height: u32, raw: &[u8]| {
        let mut pixels = raw.to_vec();
        if swap {
            pixels.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
        }
        let png = encode_png(width, height, &pixels).expect("png encode failed");
        fs::write(&path, &png).expect("failed to write png");
    };

    if backgrounds {
        // Backgrounds are one image each, and are where a GM8 game keeps its tilesets.
        for (index, slot) in assets.backgrounds.iter().enumerate() {
            if assets_written >= limit {
                break;
            }
            let Some(background) = slot else { continue };
            // Optional because the IDE lets you create a background with no image attached.
            let Some(data) = background.data.as_ref() else {
                empty += 1;
                continue;
            };
            if background.width == 0 || background.height == 0 {
                empty += 1;
                continue;
            }
            let name = unique(sanitize(&background.name.to_string()), index);
            emit(out_dir.join(format!("{}.png", name)), background.width, background.height, data);
            assets_written += 1;
            images += 1;
        }
    } else {
        for (index, slot) in assets.sprites.iter().enumerate() {
            if assets_written >= limit {
                break;
            }
            let Some(sprite) = slot else { continue };
            if sprite.frames.is_empty() {
                empty += 1;
                continue;
            }
            let name = unique(sanitize(&sprite.name.to_string()), index);
            let multi = sprite.frames.len() > 1;
            if multi {
                fs::create_dir_all(out_dir.join(&name)).expect("failed to create sprite dir");
            }
            for (n, frame) in sprite.frames.iter().enumerate() {
                if frame.width == 0 || frame.height == 0 {
                    empty += 1;
                    continue;
                }
                let path = if multi {
                    out_dir.join(&name).join(format!("{:03}.png", n))
                } else {
                    out_dir.join(format!("{}.png", name))
                };
                emit(path, frame.width, frame.height, &frame.data);
                images += 1;
            }
            assets_written += 1;
        }
    }

    println!(
        "{}: {}  images written: {}  skipped (empty): {}",
        if backgrounds { "backgrounds" } else { "sprites" },
        assets_written,
        images,
        empty
    );
}
