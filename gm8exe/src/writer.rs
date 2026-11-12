//! Writing a GM8 game back out, the counterpart to [`crate::reader`].
//!
//! Everything here is the exact inverse of something in the reader or in [`crate::gamedata::gm80`],
//! and is written to be read next to it. Where there was a choice, this file does whatever makes
//! the round trip provable rather than whatever is shortest -- a patcher that produces a file the
//! reader accepts but the real runner rejects is the failure mode that costs the most to find.

use crate::{
    GameAssets, GameVersion,
    asset::{Asset, PascalString, WritePascalString},
    gamedata::gm81,
    reader::deflate,
};
use byteorder::{LE, WriteBytesExt};
use std::io::{self, Read, Write};

/// What the reader recognises as "no asset here": a default-level deflate of four zero bytes.
///
/// Hard-coded rather than produced on the fly because the reader compares against these exact
/// bytes, so the two have to agree on the compressor's output, not merely on its meaning.
pub const DELETED_ASSET: [u8; 12] = [0x78, 0x9C, 0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00, 0x04, 0x00, 0x01];

/// Writes one asset group: a count, then each asset deflated behind its own length.
///
/// A `None` slot is written as [`DELETED_ASSET`], and a present one gets the `1` the reader tests
/// for before handing the rest to the asset's own deserialiser.
pub fn write_asset_group<T: Asset>(
    writer: &mut impl io::Write,
    assets: &[Option<Box<T>>],
    version: GameVersion,
) -> io::Result<()> {
    writer.write_u32::<LE>(assets.len() as u32)?;
    for asset in assets {
        match asset {
            None => {
                writer.write_u32::<LE>(DELETED_ASSET.len() as u32)?;
                writer.write_all(&DELETED_ASSET)?;
            },
            Some(asset) => {
                let mut plain = Vec::new();
                plain.write_u32::<LE>(1)?;
                asset.serialize_exe(&mut plain, version)?;
                let mut compressed = Vec::new();
                deflate(&plain).read_to_end(&mut compressed)?;
                writer.write_u32::<LE>(compressed.len() as u32)?;
                writer.write_all(&compressed)?;
            },
        }
    }
    Ok(())
}

/// Writes the included-files group.
///
/// Deliberately not [`write_asset_group`]: the reader parses these with `get_asset_refs` and hands
/// each inflated blob straight to the deserialiser, with none of the leading "does this exist"
/// dword every other group carries. Writing that dword here shifts every included file by four
/// bytes.
pub fn write_included_files<T: Asset>(
    writer: &mut impl io::Write,
    files: &[T],
    version: GameVersion,
) -> io::Result<()> {
    writer.write_u32::<LE>(files.len() as u32)?;
    for file in files {
        let mut plain = Vec::new();
        file.serialize_exe(&mut plain, version)?;
        let mut compressed = Vec::new();
        deflate(&plain).read_to_end(&mut compressed)?;
        writer.write_u32::<LE>(compressed.len() as u32)?;
        writer.write_all(&compressed)?;
    }
    Ok(())
}

/// Writes a block the reader will inflate whole: a length, then the deflated bytes.
pub fn write_compressed_block(writer: &mut impl io::Write, plain: &[u8]) -> io::Result<()> {
    let mut compressed = Vec::new();
    deflate(&plain).read_to_end(&mut compressed)?;
    writer.write_u32::<LE>(compressed.len() as u32)?;
    writer.write_all(&compressed)?;
    Ok(())
}

/// Writes a counted list of Pascal strings, as the library initialisation section uses.
pub fn write_pascal_strings(writer: &mut impl io::Write, strings: &[PascalString]) -> io::Result<()> {
    writer.write_u32::<LE>(strings.len() as u32)?;
    for string in strings {
        writer.write_pas_string(string)?;
    }
    Ok(())
}

/// Encrypts the asset section in place, undoing what [`crate::gamedata::gm80::decrypt`] does.
///
/// Both of the reader's passes run backwards over the data, each byte depending on one that has not
/// been touched yet, so both inverses run *forwards*. Doing either in the reader's own direction
/// produces a file that decrypts to garbage from the second byte on.
///
/// `preceding` is the byte immediately before the region -- the top byte of the length field. The
/// first byte of the section is chained from it, exactly as every later byte is chained from its
/// own predecessor, and leaving it out is the difference between a game that loads and one the
/// runner calls corrupted.
pub fn encrypt_gamedata(data: &mut [u8], swap_table: &[u8; 256], preceding: u8) {
    let len = data.len();
    if len == 0 {
        return
    }

    // Inverse of the reader's second pass: the same swaps, in the opposite order.
    for i in 0..len {
        let b = i.saturating_sub(swap_table[i & 0xFF] as usize);
        data.swap(i, b);
    }

    // Inverse of the reader's first pass, chaining each byte from the cipher byte before it.
    for i in 0..len {
        let previous = if i == 0 { preceding } else { data[i - 1] };
        let sum = data[i].wrapping_add(previous).wrapping_add(i as u8);
        data[i] = swap_table[sum as usize];
    }
}

/// Writes the swap table and encrypted asset section in the shape the reader expects to find it:
/// two garbage sizes, the table between them, then the length and the data.
///
/// No garbage is emitted *here*. This is the padding that hides the swap table, it exists only to
/// make the table harder to find, and the reader is told how much there is -- so none is as valid
/// as any other amount, and a game written with zero has been confirmed to load. Do not carry that
/// reasoning over to the garbage at the head of the body, which is not optional: see
/// [`GARBAGE_DWORDS`].
pub fn write_encrypted_gamedata(writer: &mut impl io::Write, plain: &[u8], swap_table: &[u8; 256]) -> io::Result<()> {
    writer.write_u32::<LE>(0)?; // garbage 1, in dwords
    writer.write_u32::<LE>(0)?; // garbage 2, in dwords
    writer.write_all(swap_table)?;

    let mut encrypted = plain.to_vec();
    // The byte before the section is the length field's own top byte, since that is what sits there
    // once this is written out.
    let length = encrypted.len() as u32;
    encrypt_gamedata(&mut encrypted, swap_table, (length >> 24) as u8);
    writer.write_u32::<LE>(length)?;
    writer.write_all(&encrypted)?;
    Ok(())
}

/// A swap table that is a genuine permutation of 0..=255, which is what the reader's reverse table
/// assumes: it inverts by assignment, so a table that repeats a value would silently lose bytes.
///
/// Deterministic on purpose -- a patcher should produce the same bytes twice for the same input, so
/// that "did this build change?" is answerable by comparing files.
pub fn default_swap_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        // An odd multiplier is a bijection modulo 256, so this is a permutation by construction
        // rather than by hoping.
        *slot = ((i as u32 * 37 + 11) & 0xFF) as u8;
    }
    table
}

/// How many random dwords to write at the head of the encrypted body when the original's own count
/// cannot be read -- see [`original_encryption`], which is where it normally comes from.
///
/// The head of the body is where a disagreement between this crate and the runner lands, and what
/// keeps it off bytes anyone reads. Measured against a real 8.0 runner, patching one 64MB game and
/// varying only this number *while encrypting under a swap table of our own*: 4 and 16 dwords fail
/// to load, 64 (256 bytes) and up load. 256 bytes is exactly the span the swap pass reaches back
/// over, so no table can push the disagreement past that window.
///
/// Reading the original's count is better than any choice made here, and it is what made a
/// GameMaker 8.2 game load: one that ships zero of these needs zero, because under its own swap
/// table this crate's encryption reproduces GameMaker's byte for byte and there is nothing to
/// cushion. This constant is the fallback for a file whose region header cannot be walked, and is
/// checked by `writing_a_body_leaves_room_for_the_runners_first_256_bytes`.
const GARBAGE_DWORDS: usize = 1024;

/// Deterministic stand-in for GameMaker's own garbage.
///
/// Deterministic so that "did this build change?" stays answerable by comparing two files. The
/// values are masked to 28 bits because that is the shape GameMaker's own are -- every garbage
/// dword in the games inspected sits below 0x10000000 -- and looking like a file GameMaker wrote is
/// worth more than looking random.
fn garbage(count: usize) -> impl Iterator<Item = u32> {
    let mut state: u32 = 0x8BADF00D;
    (0..count).map(move |_| {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        state >> 4
    })
}

/// Where the runner stores the *pointer* to its game data header.
///
/// The dword here is an offset, not the header -- a mistake worth naming, because splicing at this
/// address produces a file whose header check passes and whose data is nowhere to be found.
const GM80_HEADER_POINTER: usize = 0x144AC0;

/// The instruction bytes `gamedata::gm80::check` matches to decide a file is a GameMaker 8.0 runner,
/// and the offset it finds them at. The magic value follows as the operand of the CMP after them.
const GM80_LOADING_SEQUENCE_POS: usize = 0x000A49BE;
const GM80_LOADING_SEQUENCE: [u8; 8] = [0x8B, 0x45, 0xF4, 0xE8, 0x2A, 0xBD, 0xFD, 0xFF];

/// How far the runner jumps when the header is not where the pointer said, mirroring the reader.
const GM80_HEADER_STRIDE: usize = 10000;

fn read_u32_at(exe: &[u8], at: usize) -> Option<u32> {
    let bytes = exe.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Finds where a standard GameMaker 8.0 executable's data begins, by the same route the reader
/// takes: read the pointer, then hunt for the magic in strides until it turns up.
///
/// Returns the offset just past the sixteen-byte header -- magic, version, and eight bytes the
/// reader steps over -- which is where the settings chunk starts and therefore where a patcher
/// should begin writing. Returning `None` means this is not a plain 8.0 runner: UPX-packed and
/// antidec-protected games reach their data another way and are not handled here.
pub fn find_gamedata_start(exe: &[u8]) -> Option<usize> {
    if exe.get(GM80_LOADING_SEQUENCE_POS..GM80_LOADING_SEQUENCE_POS + 8)? != GM80_LOADING_SEQUENCE {
        return None
    }
    // Only the intact form is handled: where the check has been patched out with a NOP there is no
    // magic to search for, and guessing one would be worse than declining.
    if *exe.get(GM80_LOADING_SEQUENCE_POS + 8)? != 0x3D {
        return None
    }
    let magic = read_u32_at(exe, GM80_LOADING_SEQUENCE_POS + 9)?;

    let mut at = read_u32_at(exe, GM80_HEADER_POINTER)? as usize;
    loop {
        if read_u32_at(exe, at)? == magic {
            return Some(at + 16)
        }
        at += GM80_HEADER_STRIDE;
    }
}

/// Writes everything from the settings chunk to the end of the file: the whole game, minus the
/// runner executable in front of it.
///
/// Mirrors [`crate::reader::from_exe`] section for section. The order is not negotiable and there
/// are no lengths to resynchronise on once the encrypted region starts, so a section written out of
/// place does not fail here -- it fails as garbage somewhere much later.
pub fn write_gamedata(assets: &GameAssets, encryption: &Encryption) -> io::Result<Vec<u8>> {
    let version = assets.version;
    let mut out: Vec<u8> = Vec::new();

    // Settings, then the embedded DirectX DLL. Both sit outside the encrypted region.
    let mut settings = Vec::new();
    assets.settings.write(&mut settings, version)?;
    write_compressed_block(&mut out, &settings)?;
    out.write_pas_string(&assets.dx_dll_name)?;
    out.write_u32::<LE>(assets.dx_dll.len() as u32)?;
    out.write_all(&assets.dx_dll)?;

    write_encrypted_gamedata(&mut out, &gamedata_body(assets, encryption.garbage_dwords)?, &encryption.swap_table)?;
    Ok(out)
}

/// The plaintext of the encrypted region: everything from the garbage dword to the room order.
///
/// Separate from [`write_gamedata`] so it can be compared byte for byte against what a real game
/// decrypts to, which is the only way to tell a writer that is wrong from one that is merely
/// different -- our own reader will happily accept output that agrees with our own mistakes.
pub fn gamedata_body(assets: &GameAssets, garbage_dwords: usize) -> io::Result<Vec<u8>> {
    let version = assets.version;
    let mut body: Vec<u8> = Vec::new();
    body.write_u32::<LE>(garbage_dwords as u32)?;
    for word in garbage(garbage_dwords) {
        body.write_u32::<LE>(word)?;
    }
    body.write_u32::<LE>(assets.pro_flag as u32)?;
    body.write_u32::<LE>(assets.game_id)?;
    for word in assets.guid.iter() {
        body.write_u32::<LE>(*word)?;
    }

    body.write_u32::<LE>(700)?;
    body.write_u32::<LE>(assets.extensions.len() as u32)?;
    for extension in &assets.extensions {
        extension.write(&mut body, 0)?;
    }

    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.triggers, version)?;

    body.write_u32::<LE>(800)?;
    body.write_u32::<LE>(assets.constants.len() as u32)?;
    for constant in &assets.constants {
        constant.write(&mut body)?;
    }

    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.sounds, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.sprites, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.backgrounds, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.paths, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.scripts, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.fonts, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.timelines, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.objects, version)?;
    body.write_u32::<LE>(800)?;
    write_asset_group(&mut body, &assets.rooms, version)?;

    body.write_i32::<LE>(assets.last_instance_id)?;
    body.write_i32::<LE>(assets.last_tile_id)?;

    body.write_u32::<LE>(800)?;
    write_included_files(&mut body, &assets.included_files, version)?;

    body.write_u32::<LE>(800)?;
    let mut help = Vec::new();
    assets.help_dialog.write(&mut help)?;
    write_compressed_block(&mut body, &help)?;

    body.write_u32::<LE>(500)?;
    write_pascal_strings(&mut body, &assets.library_init_strings)?;

    body.write_u32::<LE>(700)?;
    body.write_u32::<LE>(assets.room_order.len() as u32)?;
    for room in assets.room_order.iter() {
        body.write_i32::<LE>(*room)?;
    }

    // Whatever followed all of that in the file this came from. See [`GameAssets::trailing`]: it is
    // padding in an 8.0 or 8.1 game and a section in an 8.2 one, and this cannot tell the two apart
    // -- which is exactly why it writes the bytes back rather than deciding.
    body.write_all(&assets.trailing)?;

    Ok(body)
}

/// How a patched executable has to be put back together, which is not the same question for the two
/// versions.
///
/// 8.0 leaves its game data in the clear, so writing one is splicing. 8.1 lays an xor stream over
/// everything from a seed-derived offset to the end of the file, so writing one is splicing and
/// then putting that stream back over the result.
pub enum Layout {
    Gm80 {
        start: usize,
    },
    /// 8.1, which carries a check on its own game data -- see [`disarm_gm81_data_check`].
    Gm81 {
        header: gm81::Header,
    },
}

impl Layout {
    /// First byte of the game data: where a patcher starts writing.
    pub fn start(&self) -> usize {
        match self {
            Layout::Gm80 { start } => *start,
            Layout::Gm81 { header } => header.data_start as usize,
        }
    }
}

/// The 8.1 runner's check on its own game data: where the branch that acts on it lives.
///
/// GameMaker 8.1 verifies the data it loads and quits without a word if any of it changed. It is
/// not subtle and it is not avoidable by writing the file more carefully: an 8.1 game rebuilt byte
/// for byte loads, and the same game with a single byte altered in the random padding at the end of
/// the body -- bytes the reader stops before and nothing ever reads, every length and offset left
/// alone -- does not. 8.0 has no such check, which is why the 8.0 patcher worked from the start.
///
/// Every 8.1 runner inspected carries `74 05 E8 B0 A7 EF FF` here: `JZ +5` over the call that gives
/// up. Turning the `JZ` into a `JMP` puts that call out of reach and the runner loads whatever it
/// is handed. This is what I Wanna Play Online's converter does, and it is how that tool patches
/// games this crate would otherwise have to decline.
const GM81_DATA_CHECK_JZ: usize = 0x10BD49;

/// Takes the 8.1 data check's failure path out of reach, in an executable already written.
///
/// Refuses rather than guesses. A byte that is neither the `JZ` nor an already-patched `JMP` means
/// this is a runner build the offset was not measured against, and writing to it would corrupt
/// machine code -- so it is left alone and the caller is told, which beats shipping a game that
/// exits on launch for a reason nobody can see.
pub fn disarm_gm81_data_check(exe: &mut [u8]) -> io::Result<()> {
    match exe.get_mut(GM81_DATA_CHECK_JZ) {
        Some(byte @ 0x74) => {
            *byte = 0xEB;
            Ok(())
        },
        // Already unconditional: another tool has been here, or this game shipped that way.
        Some(0xEB) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "this GameMaker 8.1 runner does not carry its data check where every other one does, \
             so the check cannot be disarmed and a patched build of it would not start",
        )),
    }
}

/// Identifies a standard GameMaker 8 executable and says where its data begins.
///
/// Tries the two versions in the order [`crate::gamedata::find`] does, so a file cannot be read as
/// one version and written as the other. Returning `None` means this is not a plain runner:
/// UPX-packed and antidec-protected games reach their data another way and are not handled here.
pub fn find_layout(exe: &[u8]) -> Option<Layout> {
    match find_gamedata_start(exe) {
        Some(start) => Some(Layout::Gm80 { start }),
        None => gm81::find(exe, None::<fn(&str)>).map(|header| Layout::Gm81 { header }),
    }
}

/// The swap table the executable already carries, read straight out of the encrypted region's
/// header, where it sits in the clear between the two runs of padding.
///
/// Reused rather than replaced, and this is not politeness. GameMaker writes a fresh random table
/// per build and the file says which one it used, so on the face of it any permutation should do --
/// and for 8.0 and 8.1 any does. A GameMaker 8.2 runner does not agree: it refuses a game
/// re-encrypted under a table of our own and accepts the same bytes under the table the game
/// shipped with. Whatever it does with the table beyond the documented two passes, keeping the
/// game's own is what makes the question go away. [`default_swap_table`] is the fallback for a file
/// whose header cannot be walked.
///
/// `start` is the first byte of the game data, as [`Layout::start`] reports it.
pub fn original_encryption(exe: &[u8], start: usize) -> Option<Encryption> {
    // Three length-prefixed chunks stand between the data header and the encrypted region: the
    // settings, the DirectX DLL's name, and the DLL itself.
    let mut at = start;
    for _ in 0..3 {
        at = at.checked_add(4)?.checked_add(read_u32_at(exe, at)? as usize)?;
    }
    let garbage1 = read_u32_at(exe, at)? as usize;
    let garbage2 = read_u32_at(exe, at + 4)? as usize;
    let table_at = at.checked_add(8)?.checked_add(garbage1.checked_mul(4)?)?;
    let mut swap_table = [0u8; 256];
    swap_table.copy_from_slice(exe.get(table_at..table_at + 256)?);

    // The reader inverts the table by assignment, so a table that repeats a value would silently
    // lose bytes. Every table GameMaker writes is a permutation; one that is not means the walk
    // above landed somewhere else, and inventing a table is safer than encrypting with that.
    let mut seen = [false; 256];
    for value in swap_table.iter() {
        if std::mem::replace(&mut seen[*value as usize], true) {
            return None
        }
    }

    // The body's own leading garbage count, which is the first dword of the plaintext -- readable
    // because `exe` is the buffer the reader decrypted in place.
    let body_at = table_at.checked_add(256)?.checked_add(garbage2.checked_mul(4)?)?.checked_add(4)?;
    Some(Encryption { swap_table, garbage_dwords: read_u32_at(exe, body_at)? as usize })
}

/// How a file's game data was encrypted, so that rewriting it can be done the same way.
pub struct Encryption {
    pub swap_table: [u8; 256],
    /// How many random dwords the body opens with. See [`GARBAGE_DWORDS`] for why this is carried
    /// across rather than chosen.
    pub garbage_dwords: usize,
}

/// Rebuilds a standard GameMaker 8 executable: the original runner, then freshly written data.
///
/// `exe` is the buffer [`crate::reader::from_exe`] was handed, *after* it ran. That matters for
/// 8.1: the reader decrypts in place, and an 8.1 file's xor stream starts a few bytes ahead of the
/// game data, so those bytes have to be carried across as plaintext and re-encrypted along with
/// everything written after them. Handing this the untouched original would encrypt them twice.
///
/// The runner is copied byte for byte rather than rebuilt, so everything the format check reads out
/// of its machine code -- the magic, the version, the patched-or-not jumps -- still says what it
/// said before.
pub fn to_exe(assets: &GameAssets, exe: &[u8]) -> io::Result<Vec<u8>> {
    let layout = find_layout(exe)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a plain GameMaker 8.0 or 8.1 executable"))?;
    let encryption = original_encryption(exe, layout.start())
        .unwrap_or(Encryption { swap_table: default_swap_table(), garbage_dwords: GARBAGE_DWORDS });
    let mut out = exe[..layout.start()].to_vec();
    out.extend_from_slice(&write_gamedata(assets, &encryption)?);
    if let Layout::Gm81 { header } = &layout {
        gm81::apply(&mut out, header);
        // After the xor, not before: the runner reads this as machine code, so what has to say
        // `JMP` is the byte in the finished file.
        disarm_gm81_data_check(&mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedata::gm80;
    use std::io::Cursor;

    fn is_permutation(table: &[u8; 256]) -> bool {
        let mut seen = [false; 256];
        for value in table.iter() {
            if seen[*value as usize] {
                return false
            }
            seen[*value as usize] = true;
        }
        true
    }

    #[test]
    fn the_default_swap_table_is_a_permutation() {
        assert!(is_permutation(&default_swap_table()));
    }

    /// The cushion is the whole reason the constant exists, so guard its size rather than its value:
    /// a future edit that trims it below the swap pass's 256-byte reach would produce games that
    /// load here and not in the real runner, which is the one failure this crate cannot see.
    #[test]
    fn writing_a_body_leaves_room_for_the_runners_first_256_bytes() {
        assert!(
            GARBAGE_DWORDS * 4 >= 256,
            "the head of the body must cover the 256 bytes the runner decrypts differently"
        );
        assert_eq!(garbage(GARBAGE_DWORDS).count(), GARBAGE_DWORDS);
    }

    #[test]
    fn garbage_is_deterministic_and_gamemaker_shaped() {
        assert!(garbage(64).eq(garbage(64)), "two builds of the same game must produce the same bytes");
        // Every garbage dword in the real games inspected sits below 0x10000000.
        assert!(garbage(4096).all(|word| word < 0x1000_0000));
    }

    /// The test that matters: encrypt with the writer, then decrypt with the *reader's own* code
    /// and check we get the original back. Anything less is this file agreeing with itself.
    fn round_trip_through_the_reader(plain: &[u8]) {
        let table = default_swap_table();
        let mut buffer = Vec::new();
        write_encrypted_gamedata(&mut buffer, plain, &table).expect("write");

        let mut cursor = Cursor::new(buffer.as_mut_slice());
        gm80::decrypt(&mut cursor, None::<fn(&str)>).expect("decrypt");

        // decrypt leaves the cursor just past the length field, where the plaintext now sits.
        let at = cursor.position() as usize;
        assert_eq!(&cursor.get_ref()[at..at + plain.len()], plain);
    }

    #[test]
    fn round_trips_asset_data_through_the_readers_decryptor() {
        round_trip_through_the_reader(b"the quick brown fox jumps over the lazy dog");
    }

    #[test]
    fn round_trips_every_byte_value() {
        let plain: Vec<u8> = (0..=255u8).collect();
        round_trip_through_the_reader(&plain);
    }

    #[test]
    fn round_trips_data_longer_than_the_swap_table() {
        // Past 256 bytes the second pass starts reusing table entries, which is where an off-by-one
        // in the index would first show.
        let plain: Vec<u8> = (0..5000u32).map(|i| (i.wrapping_mul(2654435761) >> 16) as u8).collect();
        round_trip_through_the_reader(&plain);
    }

    #[test]
    fn round_trips_short_and_empty_data() {
        round_trip_through_the_reader(b"");
        round_trip_through_the_reader(b"a");
        round_trip_through_the_reader(b"ab");
    }

    /// A table that repeats a value would lose bytes, so a walk that lands anywhere but on a real
    /// swap table has to say so rather than hand one back.
    #[test]
    fn a_swap_table_that_is_not_a_permutation_is_refused() {
        // Three empty chunks, no padding, then 256 bytes that are all the same.
        let mut exe = Vec::new();
        for _ in 0..3 {
            exe.extend_from_slice(&0u32.to_le_bytes());
        }
        exe.extend_from_slice(&0u32.to_le_bytes()); // garbage 1
        exe.extend_from_slice(&0u32.to_le_bytes()); // garbage 2
        exe.extend_from_slice(&[7u8; 256]);
        exe.extend_from_slice(&0u32.to_le_bytes()); // section length
        exe.extend_from_slice(&0u32.to_le_bytes()); // leading garbage count
        assert!(original_encryption(&exe, 0).is_none());
    }

    /// The table and the garbage count are the game's, and both have to come back off the walk: a
    /// GameMaker 8.2 game ships an identity table and no garbage at all, and refuses to start under
    /// anything else.
    #[test]
    fn the_files_own_table_and_garbage_count_are_read_back() {
        let table: Vec<u8> = (0..=255u8).collect();
        let mut exe = vec![0xAB; 16]; // the data starts after this
        let start = exe.len();
        exe.extend_from_slice(&4u32.to_le_bytes()); // settings length
        exe.extend_from_slice(&[0; 4]);
        exe.extend_from_slice(&2u32.to_le_bytes()); // dll name length
        exe.extend_from_slice(&[0; 2]);
        exe.extend_from_slice(&3u32.to_le_bytes()); // dll length
        exe.extend_from_slice(&[0; 3]);
        exe.extend_from_slice(&1u32.to_le_bytes()); // garbage 1, in dwords
        exe.extend_from_slice(&2u32.to_le_bytes()); // garbage 2, in dwords
        exe.extend_from_slice(&[0xEE; 4]);
        exe.extend_from_slice(&table);
        exe.extend_from_slice(&[0xEE; 8]);
        exe.extend_from_slice(&0u32.to_le_bytes()); // section length
        exe.extend_from_slice(&37u32.to_le_bytes()); // the body's leading garbage count

        let found = original_encryption(&exe, start).expect("walk");
        assert_eq!(&found.swap_table[..], &table[..]);
        assert_eq!(found.garbage_dwords, 37);
    }
}
