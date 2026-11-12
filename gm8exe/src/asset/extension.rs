use crate::{
    asset::{Error, PascalString, ReadPascalString, WritePascalString, assert_ver},
    reader::{deflate, inflate},
};
use byteorder::{LE, ReadBytesExt, WriteBytesExt};
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const VERSION: u32 = 700;

const ARG_MAX: usize = 17;

pub struct Extension {
    /// The name of the extension.
    pub name: PascalString,

    /// Name of the temporary folder to extract to at runtime.
    pub folder_name: PascalString,

    /// Files contained in the extension.
    pub files: Vec<File>,
}

pub struct File {
    /// The name of the file.
    pub name: PascalString,

    /// What kind of file this is, used to determine the usage.
    pub kind: FileKind,

    /// Initialization code for DLLs or GML files. Not used for other types.
    pub initializer: PascalString,

    /// Finalization code for DLLs or GML files. Not used for other types.
    pub finalizer: PascalString,

    /// GML or external functions you can invoke from the file.
    pub functions: Vec<FileFunction>,

    /// Constants associated with this file.
    pub consts: Vec<FileConst>,

    /// The raw filedata itself.
    pub contents: Box<[u8]>,
}

pub struct FileConst {
    pub name: PascalString,
    pub value: PascalString,
}

/// These const values are in line with the GM8 format. There is no zero.
///
/// The `Unknown` variants across these three enums exist so that a value this crate does not
/// recognise survives being written back out. Folding one into a named variant loses it: a GameMaker
/// 8.2 extension whose unused argument slots are `0` came back as `GMReal`, and the writer then put
/// `2` where the game shipped `0`. What a field means to *this* crate says nothing about what the
/// runner does with it -- the same reasoning has now produced five writer bugs.
#[derive(Copy, Clone, PartialEq)]
pub enum FileKind {
    DynamicLibrary,
    GmlScript,
    ActionLibrary,
    Other,
    Unknown(u32),
}

impl FileKind {
    /// The dword this was read from, and the one to write back.
    pub fn raw(self) -> u32 {
        match self {
            Self::DynamicLibrary => 1,
            Self::GmlScript => 2,
            Self::ActionLibrary => 3,
            Self::Other => 4,
            Self::Unknown(n) => n,
        }
    }
}

impl From<u32> for FileKind {
    fn from(n: u32) -> Self {
        match n {
            1 => Self::DynamicLibrary,
            2 => Self::GmlScript,
            3 => Self::ActionLibrary,
            4 => Self::Other,
            n => Self::Unknown(n),
        }
    }
}

/// This is in line with GM8 data and there is no value corresponding to zero.
#[derive(Copy, Clone, PartialEq)]
pub enum FunctionValueKind {
    GMString,
    GMReal,
    Unknown(u32),
}

impl FunctionValueKind {
    /// The dword this was read from, and the one to write back.
    pub fn raw(self) -> u32 {
        match self {
            Self::GMString => 1,
            Self::GMReal => 2,
            Self::Unknown(n) => n,
        }
    }
}

impl From<u32> for FunctionValueKind {
    fn from(n: u32) -> Self {
        match n {
            1 => Self::GMString,
            2 => Self::GMReal,
            n => Self::Unknown(n),
        }
    }
}

pub struct FileFunction {
    pub name: PascalString,
    pub external_name: PascalString,
    pub convention: CallingConvention,
    pub id: u32,
    pub arg_count: i32,
    pub arg_types: [FunctionValueKind; ARG_MAX],
    pub return_type: FunctionValueKind,
}

#[derive(Copy, Clone, PartialEq)]
pub enum CallingConvention {
    Gml,
    Stdcall,
    Cdecl,
    Unknown(u32),
}

impl CallingConvention {
    /// The dword this was read from, and the one to write back.
    pub fn raw(self) -> u32 {
        match self {
            Self::Gml => 2,
            Self::Stdcall => 11,
            Self::Cdecl => 12,
            Self::Unknown(n) => n,
        }
    }
}

impl From<u32> for CallingConvention {
    fn from(n: u32) -> Self {
        match n {
            2 => Self::Gml,
            11 => Self::Stdcall,
            12 => Self::Cdecl,
            n => Self::Unknown(n),
        }
    }
}

impl Extension {
    pub fn read(reader: &mut io::Cursor<&mut [u8]>, strict: bool) -> Result<Self, Error> {
        if strict {
            let version = reader.read_u32::<LE>()?;
            assert_ver(version, VERSION)?;
        } else {
            reader.seek(SeekFrom::Current(4))?;
        }

        let name = reader.read_pas_string()?;
        let folder_name = reader.read_pas_string()?;

        let file_count = reader.read_u32::<LE>()? as usize;
        let mut files: Vec<File> = (0..file_count)
            .map(|_| {
                if strict {
                    let version = reader.read_u32::<LE>()?;
                    assert_ver(version, VERSION)?;
                } else {
                    reader.seek(SeekFrom::Current(4))?;
                }

                let name = reader.read_pas_string()?;
                let kind = FileKind::from(reader.read_u32::<LE>()?);
                let initializer = reader.read_pas_string()?;
                let finalizer = reader.read_pas_string()?;

                let function_count = reader.read_u32::<LE>()? as usize;
                let functions = (0..function_count)
                    .map(|_| {
                        if strict {
                            let version = reader.read_u32::<LE>()?;
                            assert_ver(version, VERSION)?;
                        } else {
                            reader.seek(SeekFrom::Current(4))?;
                        }

                        let name = reader.read_pas_string()?;
                        let external_name = reader.read_pas_string()?;
                        let convention: CallingConvention = reader.read_u32::<LE>()?.into();

                        let id = reader.read_u32::<LE>()?;

                        let arg_count = reader.read_i32::<LE>()?;
                        let mut arg_types = [FunctionValueKind::GMReal; ARG_MAX];
                        for val in arg_types.iter_mut() {
                            *val = reader.read_u32::<LE>()?.into();
                        }
                        let return_type: FunctionValueKind = reader.read_u32::<LE>()?.into();

                        Ok(FileFunction { name, external_name, convention, id, arg_count, arg_types, return_type })
                    })
                    .collect::<Result<_, Error>>()?;

                let const_count = reader.read_u32::<LE>()? as usize;
                let consts = (0..const_count)
                    .map(|_| {
                        if strict {
                            let version = reader.read_u32::<LE>()?;
                            assert_ver(version, VERSION)?;
                        } else {
                            reader.seek(SeekFrom::Current(4))?;
                        }

                        Ok(FileConst { name: reader.read_pas_string()?, value: reader.read_pas_string()? })
                    })
                    .collect::<Result<_, Error>>()?;

                Ok(File { name, kind, initializer, finalizer, functions, consts, contents: Box::new([]) })
            })
            .collect::<Result<_, Error>>()?;

        let contents_len = reader.read_u32::<LE>()? as usize - 4;
        let seed1_raw = reader.read_u32::<LE>()?;
        let data_pos = reader.position() as usize;
        reader.seek(SeekFrom::Current(contents_len as _))?;

        // Don't do decryption if there are no contents
        if contents_len != 0 {
            // decryption setup
            let mut char_table = [0u8; 0x200];
            let mut seed1: i32 = seed1_raw as _;
            let mut seed2: i32 = (seed1 % 0xFA) + 6;
            seed1 /= 0xFA;
            if seed1 < 0 {
                seed1 += 100;
            }
            if seed2 < 0 {
                seed2 += 100;
            }
            for (i, val) in char_table.iter_mut().enumerate() {
                *val = (i % 256) as u8; // 0-255 repeating (twice)
            }

            // calculating char table - pass 1: pseudorandom byteswap
            for i in 1..0x2711 {
                let idx: usize = ((((i * seed2 as u32) + seed1 as u32) % 0xFE) + 1) as _;
                let b1 = char_table[idx];
                let b2 = char_table[idx + 1];
                char_table[idx] = b2;
                char_table[idx + 1] = b1;
            }

            // .. pass 2: use low half to scramble top half
            for i in 0..0x100 {
                let lo: u8 = char_table[i + 1];
                char_table[lo as usize + 0x100] = (i as u8).wrapping_add(1);
            }

            // decrypt data chunk
            for byte in &mut reader.get_mut()[data_pos + 1..data_pos + contents_len] {
                *byte = char_table[*byte as usize + 0x100];
            }

            let end_pos = reader.position() as usize;
            reader.set_position(data_pos as u64);

            // write file chunks
            for file in &mut files {
                if file.kind != FileKind::ActionLibrary {
                    let len = reader.read_u32::<LE>()? as usize;
                    let pos = reader.position() as usize;

                    reader.seek(SeekFrom::Current(len as i64))?; // pre-check for next get
                    let mut file_bytes = Vec::new();
                    inflate(reader.get_ref().get(pos..pos + len).unwrap_or_else(|| unreachable!()))
                        .read_to_end(&mut file_bytes)?;
                    file.contents = file_bytes.into_boxed_slice();
                }
            }

            reader.set_position(end_pos as u64);
        }

        Ok(Extension { name, folder_name, files })
    }
}

/// Builds the substitution table the file-contents blob is scrambled with.
///
/// Lifted from the reader deliberately rather than reimplemented: the two have to agree exactly,
/// and the cheapest way to guarantee that is for the writer to derive its table from the same code
/// and then invert it. The low half ends up being the encryption table and the high half the
/// decryption one, but only the latter is built explicitly -- see `encryption_table`.
fn char_table(seed1_raw: u32) -> [u8; 0x200] {
    let mut char_table = [0u8; 0x200];
    let mut seed1: i32 = seed1_raw as _;
    let mut seed2: i32 = (seed1 % 0xFA) + 6;
    seed1 /= 0xFA;
    if seed1 < 0 {
        seed1 += 100;
    }
    if seed2 < 0 {
        seed2 += 100;
    }
    for (i, val) in char_table.iter_mut().enumerate() {
        *val = (i % 256) as u8;
    }
    for i in 1..0x2711 {
        let idx: usize = ((((i * seed2 as u32) + seed1 as u32) % 0xFE) + 1) as _;
        let b1 = char_table[idx];
        let b2 = char_table[idx + 1];
        char_table[idx] = b2;
        char_table[idx + 1] = b1;
    }
    for i in 0..0x100 {
        let lo: u8 = char_table[i + 1];
        char_table[lo as usize + 0x100] = (i as u8).wrapping_add(1);
    }
    char_table
}

/// The inverse of what the reader applies, derived by inverting rather than by reasoning about the
/// table's construction. The high half is a bijection on 0..=255, so inverting it is exact, and
/// doing it this way means a future change to the table can only ever break both halves together.
fn encryption_table(seed1_raw: u32) -> [u8; 0x100] {
    let table = char_table(seed1_raw);
    let mut encrypt = [0u8; 0x100];
    for cipher in 0..0x100 {
        encrypt[table[cipher + 0x100] as usize] = cipher as u8;
    }
    encrypt
}

impl Extension {
    /// Writes this extension in the shape `read` expects.
    ///
    /// `seed1` picks the substitution table for the file-contents blob. Any value round-trips, since
    /// it is written alongside the data it scrambles; a caller with no opinion should pass 0.
    pub fn write(&self, writer: &mut impl io::Write, seed1: u32) -> io::Result<()> {
        writer.write_u32::<LE>(VERSION)?;
        writer.write_pas_string(&self.name)?;
        writer.write_pas_string(&self.folder_name)?;

        writer.write_u32::<LE>(self.files.len() as u32)?;
        for file in &self.files {
            writer.write_u32::<LE>(VERSION)?;
            writer.write_pas_string(&file.name)?;
            writer.write_u32::<LE>(file.kind.raw())?;
            writer.write_pas_string(&file.initializer)?;
            writer.write_pas_string(&file.finalizer)?;

            writer.write_u32::<LE>(file.functions.len() as u32)?;
            for function in &file.functions {
                writer.write_u32::<LE>(VERSION)?;
                writer.write_pas_string(&function.name)?;
                writer.write_pas_string(&function.external_name)?;
                writer.write_u32::<LE>(function.convention.raw())?;
                writer.write_u32::<LE>(function.id)?;
                writer.write_i32::<LE>(function.arg_count)?;
                for arg in function.arg_types.iter() {
                    writer.write_u32::<LE>(arg.raw())?;
                }
                writer.write_u32::<LE>(function.return_type.raw())?;
            }

            writer.write_u32::<LE>(file.consts.len() as u32)?;
            for constant in &file.consts {
                writer.write_u32::<LE>(VERSION)?;
                writer.write_pas_string(&constant.name)?;
                writer.write_pas_string(&constant.value)?;
            }
        }

        // The contents blob: every file that carries data, each deflated behind its own length.
        // Action libraries are skipped, exactly as the reader skips them.
        let mut contents: Vec<u8> = Vec::new();
        for file in &self.files {
            if file.kind != FileKind::ActionLibrary {
                let mut compressed = Vec::new();
                deflate(&file.contents).read_to_end(&mut compressed)?;
                contents.write_u32::<LE>(compressed.len() as u32)?;
                contents.write_all(&compressed)?;
            }
        }

        // The reader leaves byte zero of the blob alone and scrambles the rest, so this does too.
        // Getting that boundary wrong shifts the whole table by one file and shows up as garbage
        // only once a game actually loads the extension.
        if !contents.is_empty() {
            let encrypt = encryption_table(seed1);
            for byte in contents.iter_mut().skip(1) {
                *byte = encrypt[*byte as usize];
            }
        }

        // The length written counts the seed alongside the data, which is why the reader subtracts
        // four from it before using it.
        writer.write_u32::<LE>(contents.len() as u32 + 4)?;
        writer.write_u32::<LE>(seed1)?;
        writer.write_all(&contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(name: &str) -> FileFunction {
        FileFunction {
            name: name.into(),
            external_name: name.into(),
            convention: CallingConvention::Cdecl,
            id: 0,
            arg_count: 1,
            arg_types: [FunctionValueKind::GMString; ARG_MAX],
            return_type: FunctionValueKind::GMReal,
        }
    }

    fn sample(contents: &[u8]) -> Extension {
        Extension {
            name: "Http Dll 2.3".into(),
            folder_name: "http_dll_2_3".into(),
            files: vec![
                File {
                    name: "http_dll_2_3.dll".into(),
                    kind: FileKind::DynamicLibrary,
                    initializer: "".into(),
                    finalizer: "".into(),
                    functions: vec![function("socket_create"), function("socket_connect")],
                    consts: vec![FileConst { name: "socket_state_connected".into(), value: "2".into() }],
                    contents: contents.to_vec().into_boxed_slice(),
                },
                // Action libraries carry no data, and the reader skips them when unpacking the
                // contents blob -- so one has to be present for the writer's skip to be exercised.
                File {
                    name: "actions.lib".into(),
                    kind: FileKind::ActionLibrary,
                    initializer: "".into(),
                    finalizer: "".into(),
                    functions: vec![],
                    consts: vec![],
                    contents: Box::new([]),
                },
            ],
        }
    }

    fn round_trip(extension: &Extension, seed: u32) -> Extension {
        let mut buffer = Vec::new();
        extension.write(&mut buffer, seed).expect("write");
        let mut cursor = io::Cursor::new(buffer.as_mut_slice());
        Extension::read(&mut cursor, true).expect("read")
    }

    #[test]
    fn round_trips_through_the_reader() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let original = sample(&payload);
        let read = round_trip(&original, 0);

        assert_eq!(read.name.to_string(), "Http Dll 2.3");
        assert_eq!(read.folder_name.to_string(), "http_dll_2_3");
        assert_eq!(read.files.len(), 2);

        let dll = &read.files[0];
        assert_eq!(dll.name.to_string(), "http_dll_2_3.dll");
        assert!(dll.kind == FileKind::DynamicLibrary);
        assert_eq!(dll.functions.len(), 2);
        assert_eq!(dll.functions[1].name.to_string(), "socket_connect");
        assert!(dll.functions[0].convention == CallingConvention::Cdecl);
        assert_eq!(dll.consts.len(), 1);
        assert_eq!(dll.consts[0].value.to_string(), "2");

        // The point of the whole exercise: the DLL comes back byte for byte through the cipher.
        assert_eq!(dll.contents.as_ref(), payload.as_slice());
    }

    // The substitution table is seeded, and the seed travels with the data, so every seed has to
    // round-trip -- not just the one a writer happens to pick.
    #[test]
    fn round_trips_under_any_seed() {
        let payload: Vec<u8> = (0..=255u8).collect();
        for seed in [0u32, 1, 7, 250, 0xFA, 12345, 0xFFFF_FFFF] {
            let read = round_trip(&sample(&payload), seed);
            assert_eq!(read.files[0].contents.as_ref(), payload.as_slice(), "seed {seed}");
        }
    }

    #[test]
    fn round_trips_an_extension_with_no_file_data() {
        let read = round_trip(&sample(&[]), 0);
        assert_eq!(read.files.len(), 2);
        assert_eq!(read.files[0].contents.len(), 0);
    }
}
