use std::{
    io::{self, Seek, SeekFrom},
    iter::once,
};

#[derive(Copy, Clone)]
pub enum XorMethod {
    Normal,
    Sudalv,
}

/// Where a GameMaker 8.1 executable keeps its game data, and the xor layer laid over it.
///
/// Split out of [`check`] because the writer needs the same answer: an 8.1 file is written by
/// writing the data and then putting the layer back, and the stream is its own inverse, so one
/// description of it serves reading and writing both.
#[derive(Copy, Clone)]
pub struct Header {
    /// First byte the xor stream covers.
    ///
    /// Not the same as [`Header::data_start`] and often earlier than it: the offset is derived from
    /// a seed, so how much of the run-up to the game data is encrypted varies per file.
    pub encryption_start: u64,
    pub seed1: u32,
    pub seed2: u32,
    pub xor_method: XorMethod,
    /// Four bytes past the end of SUDALV's mask table. Unread by [`XorMethod::Normal`].
    pub sudalv_magic_point: u32,
    /// First byte of the game data proper -- the settings chunk's length dword.
    pub data_start: u64,
}

/// The 8.1 loading sequence, and where in the runner it sits. The dword straight after it is where
/// the runner starts looking for the data header.
const LOADING_SEQUENCE_POS: usize = 0x00226CF3;
const LOADING_SEQUENCE: [u8; 8] = [0xE8, 0x80, 0xF2, 0xDD, 0xFF, 0xC7, 0x45, 0xF0];

/// The CMP against the header magic: 125 bytes past the header-start dword.
const MAGIC_CMP_POS: usize = LOADING_SEQUENCE_POS + 12 + 125;

/// SUDALV's re-encryption, recognised by the machine code that performs it.
const SUDALV_POS: usize = 0x0010BB83;
const SUDALV_SEQUENCE: [u8; 8] = [0x8B, 0x02, 0xC1, 0xE0, 0x10, 0x8B, 0x11, 0x81];

/// The shortest file that could hold all of the above.
const MIN_LEN: usize = 0x226D8A;

fn read_u32(exe: &[u8], at: usize) -> Option<u32> {
    let bytes = exe.get(at..at + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Locates the game data in a standard gm8.1 executable without touching a byte of it.
///
/// Returns `None` when this is not one: UPX-packed and antidec-protected games reach their data
/// another way, and [`crate::gamedata::find`] handles those before it gets here.
pub fn find<F>(exe: &[u8], logger: Option<F>) -> Option<Header>
where
    F: Copy + Fn(&str),
{
    log!(logger, "Checking for standard GM8.1 format");

    // Verify size is large enough to do the following checks - otherwise it can't be this format
    if exe.len() < MIN_LEN {
        log!(logger, "File too short for this format (0x{:X} bytes)", exe.len());
        return None
    }

    // Check for the standard 8.1 loading sequence
    if exe[LOADING_SEQUENCE_POS..LOADING_SEQUENCE_POS + 8] != LOADING_SEQUENCE {
        return None
    }
    let header_start = u64::from(read_u32(exe, LOADING_SEQUENCE_POS + 8)?);

    let magic: Option<u32> = match exe[MAGIC_CMP_POS..MAGIC_CMP_POS + 3] {
        [0x81, 0x7D, 0xEC] => {
            let magic = read_u32(exe, MAGIC_CMP_POS + 3)?;
            if exe[MAGIC_CMP_POS + 7] == 0x74 {
                log!(logger, "GM8.1 magic check looks intact - value is 0x{:X}", magic);
                Some(magic)
            } else {
                log!(logger, "GM8.1 magic check's JE is patched out");
                None
            }
        },
        ref b => {
            log!(logger, "GM8.1 magic check's CMP is patched out ({:?})", b);
            None
        },
    };

    let xor_method = if exe[SUDALV_POS..SUDALV_POS + 8] == SUDALV_SEQUENCE {
        log!(logger, "Found SUDALV re-encryption");
        XorMethod::Sudalv
    } else {
        XorMethod::Normal
    };

    // With the CMP intact the runner hunts for the magic in strides from `header_start`; with it
    // patched out there is nothing to hunt for and the header is simply there.
    let magic_pos = match magic {
        Some(n) => {
            log!(logger, "Searching for GM8.1 magic number {} from position {}", n, header_start);
            match find_value(exe, header_start, n) {
                Some(pos) => pos,
                None => {
                    log!(logger, "Didn't find GM81 magic value (0x{:X}) before EOF, so giving up", n);
                    return None
                },
            }
        },
        None => header_start,
    };

    header_at(exe, magic_pos, xor_method)
}

/// Check if this is a standard gm8.1 game by looking for the loading sequence
/// If so, removes gm81 encryption and sets the cursor to the start of the gamedata.
pub fn check<F>(exe: &mut io::Cursor<&mut [u8]>, logger: Option<F>) -> io::Result<bool>
where
    F: Copy + Fn(&str),
{
    let header = match find(exe.get_ref(), logger) {
        Some(header) => header,
        None => return Ok(false),
    };
    log!(logger, "Decrypting GM8.1 protection (seed1: {}, seed2: {})", header.seed1, header.seed2);
    apply(exe.get_mut(), &header);
    exe.set_position(header.data_start);
    Ok(true)
}

/// Check if this is a standard gm8.1 game by looking for the default header (last-resort method)
/// If so, removes gm81 encryption and sets the cursor to the start of the gamedata.
pub fn check_lazy<F>(exe: &mut io::Cursor<&mut [u8]>, logger: Option<F>) -> io::Result<bool>
where
    F: Copy + Fn(&str),
{
    log!(logger, "Searching for default GM8.1 data header");
    exe.set_position(3800004);
    let found_header = seek_value(exe, 0xF7140067)?.is_some();
    if found_header {
        decrypt(exe, logger, XorMethod::Normal)?;
        exe.seek(SeekFrom::Current(20))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Scans for a GM81-style magic value from `from`, returning the offset of the eight bytes it was
/// parsed from -- the value is spread across two dwords by byte, which is the whole trick.
pub fn find_value(exe: &[u8], from: u64, value: u32) -> Option<u64> {
    let mut pos = from;
    loop {
        let d1 = read_u32(exe, pos as usize)?;
        let d2 = read_u32(exe, pos as usize + 4)?;
        if ((d1 & 0xFF00FF00) | (d2 & 0x00FF00FF)) == value {
            break Some(pos)
        }
        pos += 1;
        if ((pos + 8) as usize) >= exe.len() {
            break None
        }
    }
}

/// Seeks for a GM81-style magic value from its current position.
/// Returns the associated xor value within the magic if it was found; returns None otherwise.
/// On success, the cursor will have been advanced just past the eight bytes from which the value was parsed.
pub fn seek_value(exe: &mut io::Cursor<&mut [u8]>, value: u32) -> io::Result<Option<u32>> {
    let found = find_value(exe.get_ref(), exe.position(), value);
    Ok(found.map(|pos| {
        exe.set_position(pos + 8);
        let d1 = read_u32(exe.get_ref(), pos as usize).unwrap_or(0);
        let d2 = read_u32(exe.get_ref(), pos as usize + 4).unwrap_or(0);
        (d1 & 0x00FF00FF) | (d2 & 0xFF00FF00)
    }))
}

/// YYG's crc32 implementation, which is what the two xor seeds are derived through.
fn crc_32(hash_key: &[u8], crc_table: &[u32; 256]) -> u32 {
    let mut result: u32 = 0xFFFFFFFF;
    for c in hash_key.iter() {
        result = (result >> 8) ^ crc_table[((result & 0xFF) as u8 ^ c) as usize];
    }
    result
}

fn crc_32_reflect(mut value: u32, c: i8) -> u32 {
    let mut rvalue: u32 = 0;
    for i in 1..=c {
        if value & 1 != 0 {
            rvalue |= 1 << (c - i);
        }
        value >>= 1;
    }
    rvalue
}

fn crc_table() -> [u32; 256] {
    let mut crc_table = [0u32; 256];
    let crc_polynomial: u32 = 0x04C11DB7;
    for (i, val) in crc_table.iter_mut().enumerate() {
        *val = crc_32_reflect(i as u32, 8) << 24;
        for _ in 0..8 {
            *val = (*val << 1) ^ if *val & (1 << 31) != 0 { crc_polynomial } else { 0 };
        }
        *val = crc_32_reflect(*val, 32);
    }
    crc_table
}

/// Derives the xor layer from the bytes that follow the magic at `magic_pos`.
///
/// Public because [`crate::gamedata::find`]'s antidec paths reach this same point by a different
/// route: they find the magic with [`seek_value`] rather than off the loading sequence.
pub fn header_at(exe: &[u8], magic_pos: u64, xor_method: XorMethod) -> Option<Header> {
    let after_magic = magic_pos + 8;
    let hash_key = format!("_MJD{}#RWK", read_u32(exe, after_magic as usize)?);
    let hash_key_utf16: Vec<u8> = hash_key.bytes().flat_map(|c| once(c).chain(once(0))).collect();

    // get our two seeds for generating xor masks
    let seed1 = read_u32(exe, after_magic as usize + 4)?;
    let seed2 = crc_32(&hash_key_utf16, &crc_table());

    Some(Header {
        // Where the encryption starts is itself derived from a seed, so it moves file to file.
        encryption_start: after_magic + 8 + u64::from(seed2 & 0xFF) + 10,
        seed1,
        seed2,
        xor_method,
        sudalv_magic_point: (magic_pos - 4) as u32,
        // Twenty bytes the runner steps over sit between the seeds and the settings chunk.
        data_start: after_magic + 8 + 20,
    })
}

/// Puts the xor layer over `exe`, or takes it off again: the stream is its own inverse.
pub fn apply(exe: &mut [u8], header: &Header) {
    // Make the seed-cycling iterator
    let mut generator: Box<dyn Iterator<Item = u32>> = match header.xor_method {
        XorMethod::Normal => Box::new(NormalMaskGenerator { seed1: header.seed1, seed2: header.seed2 }),
        XorMethod::Sudalv => {
            let mask_data = &exe[..(header.sudalv_magic_point + 4) as usize];
            let mask_count = mask_data
                .rchunks_exact(2)
                .skip(1)
                .zip(mask_data.rchunks_exact(2))
                .position(|xy| xy == (&[0, 0], &[0, 0]))
                .unwrap();
            let iter = mask_data
                .rchunks_exact(2)
                .skip(1)
                .map(|x| u16::from_le_bytes([x[0], x[1]]))
                .take(mask_count + 1)
                .collect::<Vec<u16>>()
                .into_iter()
                .cycle();
            Box::new(SudalvMaskGenerator { seed1: header.seed1, seed2: header.seed2, iter })
        },
    };

    let game_data = &mut exe[header.encryption_start as usize..];
    let array_hack = |slice| <&mut [u8] as TryInto<&mut [u8; 4]>>::try_into(slice).unwrap();
    for chunk in game_data.chunks_exact_mut(4).map(array_hack) {
        let dword = u32::from_le_bytes(*chunk);
        *chunk = (dword ^ generator.next().unwrap()).to_le_bytes();
    }
}

/// Removes GM8.1 encryption in-place, from a cursor sitting just past the magic -- where
/// [`seek_value`] leaves it -- and leaves it just past the two seeds.
pub fn decrypt<F>(data: &mut io::Cursor<&mut [u8]>, logger: Option<F>, xor_method: XorMethod) -> io::Result<Header>
where
    F: Copy + Fn(&str),
{
    let magic_pos = data.position() - 8;
    let header = header_at(data.get_ref(), magic_pos, xor_method)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "GM8.1 header runs off the end of the file"))?;

    log!(logger, "Decrypting GM8.1 protection (seed1: {}, seed2: {})", header.seed1, header.seed2);

    apply(data.get_mut(), &header);
    data.set_position(magic_pos + 16);
    Ok(header)
}

// it's all just xor mask generator code below here

struct NormalMaskGenerator {
    seed1: u32,
    seed2: u32,
}
impl Iterator for NormalMaskGenerator {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.seed1 = (0xFFFF & self.seed1) * 0x9069 + (self.seed1 >> 16);
        self.seed2 = (0xFFFF & self.seed2) * 0x4650 + (self.seed2 >> 16);
        Some((self.seed1 << 16) + (self.seed2 & 0xFFFF))
    }
}

struct SudalvMaskGenerator<I: Iterator<Item = u16>> {
    seed1: u32,
    seed2: u32,
    iter: I,
}
impl<I: Iterator<Item = u16>> Iterator for SudalvMaskGenerator<I> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        self.seed1 = (0xFFFF & self.seed1) * u32::from(self.iter.next().unwrap()) + (self.seed1 >> 16);
        self.seed2 = (0xFFFF & self.seed2) * u32::from(self.iter.next().unwrap()) + (self.seed2 >> 16);
        Some((self.seed1 << 16) + (self.seed2 & 0xFFFF))
    }
}
