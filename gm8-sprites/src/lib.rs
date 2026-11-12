//! Sprite extraction from a GameMaker 8 executable, delegated to `gm8exe`'s reader so the sprites
//! read here are the ones the game itself loads.
//!
//! Modelled on `gm8-detect`, down to the plain C ABI: no wasm-bindgen, so the host needs no
//! generated glue and no environment-specific loader.
//!
//! Unlike that crate this calls `reader::from_exe`, which parses every asset rather than sniffing a
//! header. That is the expensive part, so it happens once per upload and the parsed game is kept
//! in this instance; the host then asks for names, and for the pixels of only the few sprites a
//! player actually chose. `multithread: false`, because wasm32-unknown-unknown has no threads for
//! rayon to use.
//!
//! # Results
//!
//! A call that produces bytes leaves them in one buffer owned by this module, which the host reads
//! through `result_ptr`/`result_len` before making the next call. One buffer rather than returning
//! allocations the host would have to free: there is exactly one caller, it is single-threaded,
//! and it copies the bytes straight into JS.

use gm8exe::reader;

/// The parsed game, alive between calls. Single-threaded by construction (wasm32 has one thread),
/// and only ever touched through the functions below.
static mut GAME: Option<gm8exe::GameAssets> = None;
static mut RESULT: Vec<u8> = Vec::new();

/// Reserves `len` bytes for the host to write an executable into. No free: the instance is
/// discarded once its sprites have been read.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Parses the executable in `[ptr, ptr + len)` and keeps it.
///
/// Returns how many sprite slots the game has, or -1 if it is not a GM8 executable this crate can
/// read. The count includes empty slots, which `names` reports as blank -- slot numbers are the
/// ids GML uses, so they have to stay stable.
///
/// # Safety
/// `ptr` must be a pointer returned by `alloc(len)` with `len` bytes written into it.
#[no_mangle]
pub unsafe extern "C" fn load(ptr: *mut u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts_mut(ptr, len);
    let logger = Option::<fn(&str)>::None;

    // strict=false, the same choice the dump_assets example makes: fangames are often built by
    // tools that bend the format slightly, and a hard failure would lose every sprite over one odd
    // asset. multithread=false: no threads here for rayon to spread across.
    match reader::from_exe(bytes, logger, false, false) {
        Ok(assets) => {
            let count = assets.sprites.len() as i32;
            GAME = Some(assets);
            count
        }
        Err(_) => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn result_ptr() -> *const u8 {
    RESULT.as_ptr()
}

#[no_mangle]
pub unsafe extern "C" fn result_len() -> usize {
    RESULT.len()
}

/// Every sprite's name, newline-separated, in slot order. An empty slot contributes an empty line,
/// so the nth line is always the name of sprite n.
///
/// Names only, because this is what the host needs to work out which sprites are worth showing,
/// and the pixels of a whole game are tens of megabytes it would then throw away.
///
/// # Safety
/// `load` must have returned a count >= 0.
#[no_mangle]
pub unsafe extern "C" fn names() -> i32 {
    let Some(game) = GAME.as_ref() else { return -1 };
    let mut out = Vec::new();
    for (index, slot) in game.sprites.iter().enumerate() {
        if index > 0 {
            out.push(b'\n');
        }
        if let Some(sprite) = slot {
            // A GM8 asset name is a Pascal string of bytes; anything that is not valid UTF-8 is
            // replaced rather than dropping the sprite, so its slot keeps its line.
            out.extend_from_slice(String::from_utf8_lossy(sprite.name.0.as_ref()).as_bytes());
        }
    }
    RESULT = out;
    0
}

/// One sprite's frames, as:
///
/// ```text
/// i32 origin_x, i32 origin_y, u32 width, u32 height, u32 frame_count
/// frame_count * (width * height * 4 bytes, RGBA)
/// ```
///
/// Frames of a GM8 sprite are all the same size, so the dimensions are written once. The bytes are
/// swapped from the BGRA the loader hands back (GM8 stores Windows DIB order, whatever gm8exe's
/// own sprite docs claim -- the dump_assets example makes the same correction).
///
/// Returns 0, or -1 for an empty slot, an out-of-range index, or a sprite with no frames.
///
/// # Safety
/// `load` must have returned a count >= 0.
#[no_mangle]
pub unsafe extern "C" fn frames(index: u32) -> i32 {
    let Some(game) = GAME.as_ref() else { return -1 };
    let Some(Some(sprite)) = game.sprites.get(index as usize) else { return -1 };
    let Some(first) = sprite.frames.first() else { return -1 };
    if first.width == 0 || first.height == 0 {
        return -1;
    }

    let mut out = Vec::new();
    out.extend_from_slice(&sprite.origin_x.to_le_bytes());
    out.extend_from_slice(&sprite.origin_y.to_le_bytes());
    out.extend_from_slice(&first.width.to_le_bytes());
    out.extend_from_slice(&first.height.to_le_bytes());
    out.extend_from_slice(&(sprite.frames.len() as u32).to_le_bytes());
    for frame in &sprite.frames {
        // A frame that disagrees with the first is written as transparent rather than shifting
        // every frame after it by its own length -- the host reads fixed-size frames.
        if frame.width != first.width || frame.height != first.height {
            out.resize(out.len() + (first.width * first.height * 4) as usize, 0);
            continue;
        }
        let at = out.len();
        out.extend_from_slice(&frame.data);
        out[at..].chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
    }
    RESULT = out;
    0
}
