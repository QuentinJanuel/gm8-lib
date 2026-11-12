//! GM8 format detection, delegated to `gm8exe` itself so the answer cannot drift from what the
//! reader accepts. It replaces a ~700-line TypeScript port that did drift, declining UPX-packed
//! antidec2 games that play fine.
//!
//! `reader::read_pe_header` and `gamedata::find` are called unmodified; nothing is reimplemented.
//! Not `reader::from_exe`, which would parse every asset to answer a yes/no question and pulls in
//! rayon, which has no threads on wasm32. Plain C ABI, so the host needs no glue and no
//! environment-specific loader.

use gm8exe::{gamedata, reader, GameVersion};
use std::io::Cursor;

/// The wire format the host maps back to a version. Append only, never renumber.
const GM80: i32 = 0;
const GM81: i32 = 1;
/// Not a GM8 executable this crate can load. Not the same claim as "not GM8" — see the host's docs.
const NONE: i32 = -1;

/// Reserves `len` bytes for the host to write an executable into. No free: the instance is
/// discarded after one `detect`.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::with_capacity(len);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Identifies the executable in `[ptr, ptr + len)`, returning one of the constants above.
///
/// # Safety
/// `ptr` must be a pointer returned by `alloc(len)` with `len` bytes written into it.
#[no_mangle]
pub unsafe extern "C" fn detect(ptr: *mut u8, len: usize) -> i32 {
    let bytes = std::slice::from_raw_parts_mut(ptr, len);

    // `find` decrypts in place on the antidec2/gm81 paths -- wasm-local memory the host never
    // reads back, so the caller's buffer is untouched.
    let mut exe = Cursor::new(bytes);
    let logger = Option::<fn(&str)>::None;

    // `find` takes the UPX decision as an argument rather than making it.
    let upx_data = match reader::read_pe_header(&mut exe, logger) {
        Ok(header) => header.upx_data,
        Err(_) => return NONE,
    };

    match gamedata::find(&mut exe, logger, upx_data) {
        Ok(GameVersion::GameMaker8_0) => GM80,
        Ok(GameVersion::GameMaker8_1) => GM81,
        Err(_) => NONE, // every failure means "not a file I understand"

    }
}
