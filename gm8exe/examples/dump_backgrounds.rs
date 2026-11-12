//! Lists every Background in a GM8 exe with its pixel dimensions.
//!
//! GM8 hands `background_get_texture` a texture *page*, padded to a power of two, so a game's
//! shader that samples a background by name is really sampling that padded page -- while this
//! emulator packs backgrounds into a shared atlas at their exact size. Whenever a shader indexes a
//! background by texel (a colour LUT, a gradient ramp, a tile sheet), the difference decides whether
//! the lookup lands. This answers the first question: how big is the image really.
//!
//! Usage: cargo run --example dump_backgrounds -p gm8exe -- <path-to-exe> [name-filter]

use std::fs;

fn main() {
    let exe_path = std::env::args().nth(1).expect("usage: dump_backgrounds <exe> [name-filter]");
    let filter = std::env::args().nth(2);

    let mut bytes = fs::read(&exe_path).expect("failed to read exe");
    let assets = gm8exe::reader::from_exe(&mut bytes, None::<fn(&str)>, false, true).expect("failed to parse exe");

    // The version decides whether `d3d_model_draw` caches its vertices (8.1) or replays the model
    // every time (8.0), which changes what a model's baked texture coordinates mean.
    println!("version: {:?}", assets.version);
    println!("{} background slot(s)", assets.backgrounds.len());
    for (i, background) in assets.backgrounds.iter().enumerate() {
        let Some(background) = background else { continue };
        let name = background.name.to_string();
        if let Some(filter) = &filter {
            if !name.to_lowercase().contains(&filter.to_lowercase()) {
                continue
            }
        }
        let (w, h) = (background.width, background.height);
        let pot = |v: u32| v != 0 && v & (v - 1) == 0;
        // The centre pixel, so a dump can be matched against what the renderer reports reading back
        // out of the atlas for a given rect. BGRA in the file, printed as RGBA.
        let centre = background.data.as_ref().and_then(|data| {
            let offset = ((h / 2) * w + w / 2) as usize * 4;
            data.get(offset..offset + 4).map(|px| format!("rgba({},{},{},{})", px[2], px[1], px[0], px[3]))
        });
        println!(
            "{i:4}  {name:<40} {w}x{h}{}  centre={}",
            if pot(w) && pot(h) { "" } else { "  (NOT power-of-two: GM8 would pad this)" },
            centre.as_deref().unwrap_or("<no data>"),
        );
    }
}
