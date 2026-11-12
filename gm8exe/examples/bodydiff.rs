//! Compares the game data this crate *writes* against the game data a real GameMaker 8 exe
//! *contains*, section by section, so that a writer bug is a line of output rather than a game that
//! silently refuses to load.
//!
//! Three of the writer's bugs were found this way and one was not findable any other way: an asset
//! is a length-delimited compressed blob, so a short write leaves trailing bytes that this crate's
//! own reader ignores. Reader and writer then agree with each other while both disagree with
//! GameMaker, and only a comparison against an untouched original shows it.
//!
//! What it does *not* prove is worth stating: agreement here means our reader and writer are
//! mutual inverses on this file. It says nothing about whether either matches the runner. For that
//! there is no substitute for running the game -- see `GARBAGE_DWORDS` in `writer.rs` for a
//! divergence that this tool reports as a clean match.
//!
//! The settings chunk and the embedded DirectX DLL are compared too, even though neither is part of
//! the body. They sit between the data header and the encrypted region, and leaving them out is how
//! a writer bug hid: the chunk optionally ends with two dwords, the writer always wrote them, and
//! the eight extra bytes were enough to stop a game loading with every section below reported clean.
//!
//! Usage: cargo run --release --example bodydiff -p gm8exe -- <path-to-exe>

use byteorder::{LE, ReadBytesExt};
use flate2::bufread::ZlibDecoder;
use gm8exe::{
    gamedata::{gm80, gm81},
    reader, writer,
};
use std::io::{self, Cursor, Read, Seek, SeekFrom};

/// Walks the settings and DLL chunks to find where the encrypted region starts, then reads its
/// header. Deliberately not a call into the reader: this has to describe the file's *layout*, which
/// the reader consumes and discards.
struct Layout {
    /// First byte of the encrypted-region header (the garbage1 count).
    region: usize,
    /// First byte of the encrypted body, and its length.
    body_start: usize,
    body_len: usize,
}

fn layout(exe: &[u8]) -> Layout {
    let data_start = writer::find_layout(exe).expect("not a plain GameMaker 8.0 or 8.1 executable").start();
    let mut c = Cursor::new(exe);
    c.set_position(data_start as u64);
    let settings_len = c.read_u32::<LE>().unwrap() as i64;
    c.seek(SeekFrom::Current(settings_len)).unwrap();
    let name_len = c.read_u32::<LE>().unwrap() as i64;
    c.seek(SeekFrom::Current(name_len)).unwrap();
    let dll_len = c.read_u32::<LE>().unwrap() as i64;
    c.seek(SeekFrom::Current(dll_len)).unwrap();

    let region = c.position() as usize;
    let garbage1 = c.read_u32::<LE>().unwrap();
    let garbage2 = c.read_u32::<LE>().unwrap();
    c.seek(SeekFrom::Current(garbage1 as i64 * 4)).unwrap();
    c.seek(SeekFrom::Current(256)).unwrap();
    c.seek(SeekFrom::Current(garbage2 as i64 * 4)).unwrap();
    let body_len = c.read_u32::<LE>().unwrap() as usize;
    Layout { region, body_start: c.position() as usize, body_len }
}

fn original_body(exe: &mut [u8]) -> Vec<u8> {
    // 8.1 lays an xor stream over the whole tail of the file. Take it off and what is underneath is
    // laid out exactly the way 8.0 is, so everything below this line is version-blind.
    if let Some(writer::Layout::Gm81 { header }) = writer::find_layout(exe) {
        gm81::apply(exe, &header);
    }
    let l = layout(exe);
    let mut c = Cursor::new(exe);
    c.set_position(l.region as u64);
    gm80::decrypt(&mut c, None::<fn(&str)>).expect("decrypt");
    c.get_ref()[l.body_start..l.body_start + l.body_len].to_vec()
}

fn inflate_all(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = ZlibDecoder::new(data).read_to_end(&mut out);
    out
}

enum Chunk {
    /// A run of bytes compared verbatim.
    Raw(Vec<u8>),
    /// A version dword or a count.
    Word(u32),
    /// A counted group of length-prefixed deflated blobs.
    Group(Vec<Vec<u8>>),
    /// One length-prefixed deflated blob.
    Blob(Vec<u8>),
}

struct Section {
    name: String,
    offset: usize,
    chunk: Chunk,
}

struct Walker<'a> {
    c: Cursor<&'a [u8]>,
    out: Vec<Section>,
}

impl<'a> Walker<'a> {
    fn pos(&self) -> usize {
        self.c.position() as usize
    }

    fn push(&mut self, name: &str, offset: usize, chunk: Chunk) {
        self.out.push(Section { name: name.into(), offset, chunk });
    }

    fn raw(&mut self, name: &str, n: usize) -> io::Result<()> {
        let at = self.pos();
        let mut buf = vec![0u8; n];
        self.c.read_exact(&mut buf)?;
        self.push(name, at, Chunk::Raw(buf));
        Ok(())
    }

    fn word(&mut self, name: &str) -> io::Result<u32> {
        let at = self.pos();
        let v = self.c.read_u32::<LE>()?;
        self.push(name, at, Chunk::Word(v));
        Ok(v)
    }

    fn group(&mut self, name: &str) -> io::Result<()> {
        let at = self.pos();
        let count = self.c.read_u32::<LE>()? as usize;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let len = self.c.read_u32::<LE>()? as usize;
            let p = self.pos();
            self.c.seek(SeekFrom::Current(len as i64))?;
            items.push(self.c.get_ref()[p..p + len].to_vec());
        }
        self.push(name, at, Chunk::Group(items));
        Ok(())
    }

    fn blob(&mut self, name: &str) -> io::Result<()> {
        let at = self.pos();
        let len = self.c.read_u32::<LE>()? as usize;
        let p = self.pos();
        self.c.seek(SeekFrom::Current(len as i64))?;
        let data = self.c.get_ref()[p..p + len].to_vec();
        self.push(name, at, Chunk::Blob(data));
        Ok(())
    }

    /// A Pascal string, skipped rather than kept -- the sections built out of these are compared as
    /// one raw run.
    fn pas(&mut self) -> io::Result<()> {
        let len = self.c.read_u32::<LE>()? as i64;
        self.c.seek(SeekFrom::Current(len))?;
        Ok(())
    }

    /// Records everything the closure consumed as one raw section. For the parts of the body that
    /// have no length prefix to resynchronise on, so a mismatch is reported rather than chased.
    fn span(&mut self, name: &str, body: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        let at = self.pos();
        body(self)?;
        let bytes = self.c.get_ref()[at..self.pos()].to_vec();
        self.push(name, at, Chunk::Raw(bytes));
        Ok(())
    }
}

/// Mirrors `writer::gamedata_body` section for section, which is the point: the two have to be read
/// side by side, and a section added to one without the other shows up as a misaligned offset.
fn walk(body: &[u8]) -> io::Result<Vec<Section>> {
    let mut w = Walker { c: Cursor::new(body), out: Vec::new() };

    let at = w.pos();
    let garbage = w.c.read_u32::<LE>()?;
    w.c.seek(SeekFrom::Current(garbage as i64 * 4))?;
    w.push("garbage", at, Chunk::Word(garbage));

    w.raw("pro/game_id/guid", 24)?;

    w.word("extensions ver")?;
    w.span("extensions", |w| {
        let count = w.c.read_u32::<LE>()?;
        for _ in 0..count {
            w.c.read_u32::<LE>()?; // version
            w.pas()?; // name
            w.pas()?; // folder
            let files = w.c.read_u32::<LE>()?;
            for _ in 0..files {
                w.c.read_u32::<LE>()?; // version
                w.pas()?; // name
                w.c.read_u32::<LE>()?; // kind
                w.pas()?; // initializer
                w.pas()?; // finalizer
                let functions = w.c.read_u32::<LE>()?;
                for _ in 0..functions {
                    w.c.read_u32::<LE>()?; // version
                    w.pas()?; // name
                    w.pas()?; // external name
                    // convention, id, arg count, 17 arg types, return type
                    w.c.seek(SeekFrom::Current(4 * 21))?;
                }
                let consts = w.c.read_u32::<LE>()?;
                for _ in 0..consts {
                    w.c.read_u32::<LE>()?; // version
                    w.pas()?; // name
                    w.pas()?; // value
                }
            }
            let contents = w.c.read_u32::<LE>()? as i64;
            w.c.seek(SeekFrom::Current(contents))?;
        }
        Ok(())
    })?;

    w.word("triggers ver")?;
    w.group("triggers")?;

    w.word("constants ver")?;
    w.span("constants", |w| {
        let count = w.c.read_u32::<LE>()?;
        for _ in 0..count {
            w.pas()?; // name
            w.pas()?; // expression
        }
        Ok(())
    })?;

    for name in ["sounds", "sprites", "backgrounds", "paths", "scripts", "fonts", "timelines", "objects", "rooms"] {
        w.word(&format!("{name} ver"))?;
        w.group(name)?;
    }

    w.raw("last ids", 8)?;

    w.word("included files ver")?;
    w.group("included files")?;

    w.word("help ver")?;
    w.blob("help")?;

    w.word("library init ver")?;
    w.span("library init", |w| {
        let count = w.c.read_u32::<LE>()?;
        for _ in 0..count {
            w.pas()?;
        }
        Ok(())
    })?;

    w.word("room order ver")?;
    w.span("room order", |w| {
        let count = w.c.read_u32::<LE>()? as i64;
        w.c.seek(SeekFrom::Current(count * 4))?;
        Ok(())
    })?;

    // Real games leave thousands of random bytes here. The runner never reads them.
    let at = w.pos();
    let rest = w.c.get_ref()[at..].to_vec();
    w.push("tail padding", at, Chunk::Raw(rest));

    Ok(w.out)
}

fn describe(chunk: &Chunk) -> String {
    match chunk {
        Chunk::Raw(b) => format!("{} bytes", b.len()),
        Chunk::Word(v) => format!("dword {v}"),
        Chunk::Group(items) => format!("{} items", items.len()),
        Chunk::Blob(b) => format!("{} bytes deflated", b.len()),
    }
}

/// Compares deflated data by what it inflates to, never by its bytes: our compressor and
/// GameMaker's produce different output for the same input, and that difference is harmless. A
/// difference in what comes *out* is not.
fn same(a: &Chunk, b: &Chunk) -> bool {
    match (a, b) {
        (Chunk::Raw(p), Chunk::Raw(q)) => p == q,
        (Chunk::Word(p), Chunk::Word(q)) => p == q,
        (Chunk::Blob(p), Chunk::Blob(q)) => inflate_all(p) == inflate_all(q),
        (Chunk::Group(p), Chunk::Group(q)) => {
            p.len() == q.len() && p.iter().zip(q).all(|(i, j)| i == j || inflate_all(i) == inflate_all(j))
        },
        _ => false,
    }
}

fn report(original: &[Section], ours: &[Section]) -> usize {
    let mut differing = 0;
    println!("{:<20} {:>12} {:>12}  {:<22} {:<22}", "section", "orig off", "ours off", "original", "ours");
    for (x, y) in original.iter().zip(ours) {
        let ok = same(&x.chunk, &y.chunk);
        if !ok {
            differing += 1;
        }
        println!(
            "{:<20} {:>12} {:>12}  {:<22} {:<22}{}",
            x.name,
            x.offset,
            y.offset,
            describe(&x.chunk),
            describe(&y.chunk),
            if ok { "" } else { "   <-- DIFFERS" }
        );
        match (&x.chunk, &y.chunk) {
            (Chunk::Group(p), Chunk::Group(q)) if !ok => {
                for (i, (m, n)) in p.iter().zip(q).enumerate() {
                    let (mi, ni) = (inflate_all(m), inflate_all(n));
                    if mi != ni {
                        let at = mi.iter().zip(&ni).position(|(u, v)| u != v);
                        println!(
                            "      #{i}: inflates to {} vs {} bytes, first difference at {at:?}",
                            mi.len(),
                            ni.len()
                        );
                    }
                }
            },
            (Chunk::Raw(p), Chunk::Raw(q)) if !ok => match p.iter().zip(q).position(|(u, v)| u != v) {
                Some(at) => {
                    let end = |v: &Vec<u8>| (at + 16).min(v.len());
                    println!("      first difference at {at}");
                    println!("        original {:02X?}", &p[at..end(p)]);
                    println!("        ours     {:02X?}", &q[at..end(q)]);
                },
                None => println!("      identical up to {} vs {} bytes", p.len(), q.len()),
            },
            _ => {},
        }
    }
    if original.len() != ours.len() {
        println!("section count differs: {} vs {}", original.len(), ours.len());
    }
    differing
}

/// Compares the two chunks that sit ahead of the encrypted region: the settings, and the DirectX
/// DLL the runner extracts by name. Deflated data is compared by what it inflates to, as everywhere
/// else here.
fn head_report(exe: &mut [u8], assets: &gm8exe::GameAssets) {
    let layout = writer::find_layout(exe).expect("layout");
    if let writer::Layout::Gm81 { header } = &layout {
        gm81::apply(exe, header);
    }
    let mut c = Cursor::new(&exe[..]);
    c.set_position(layout.start() as u64);
    let settings_len = c.read_u32::<LE>().unwrap() as usize;
    let at = c.position() as usize;
    let settings = inflate_all(&exe[at..at + settings_len]);
    c.seek(SeekFrom::Current(settings_len as i64)).unwrap();
    let name_len = c.read_u32::<LE>().unwrap() as i64;
    c.seek(SeekFrom::Current(name_len)).unwrap();
    let dll_len = c.read_u32::<LE>().unwrap() as usize;
    let at = c.position() as usize;
    let dll = &exe[at..at + dll_len];

    let mut ours = Vec::new();
    assets.settings.write(&mut ours, assets.version).expect("settings");
    let same_dll = assets.dx_dll.len() == dll.len() && assets.dx_dll == dll;
    println!("settings: original {} bytes, ours {} bytes -- {}", settings.len(), ours.len(), match settings
        .iter()
        .zip(&ours)
        .position(|(a, b)| a != b)
    {
        None if settings.len() == ours.len() => "identical".to_string(),
        None => format!("one is a prefix of the other, {} bytes apart", ours.len().abs_diff(settings.len())),
        Some(at) => format!("DIFFERS at dword {}", at / 4),
    });
    println!("dll: {}\n", if same_dll { "identical" } else { "DIFFERS" });
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: bodydiff <path-to-exe>");
    let mut exe = std::fs::read(&path).expect("could not read the executable");

    // `from_exe` decrypts in place, so it gets its own copy of the bytes.
    let assets = reader::from_exe(exe.clone(), None::<fn(&str)>, false, true).expect("could not read the game");
    // The same encryption the writer would pick for this game, so the garbage row lines up and a
    // difference below it is a real one.
    let start = writer::find_layout(&exe).expect("layout").start();
    let mut plain = exe.clone();
    let _ = reader::from_exe(&mut plain, None::<fn(&str)>, false, true).expect("could not read the game");
    let encryption = writer::original_encryption(&plain, start).expect("could not read this game's own encryption");
    let ours = writer::gamedata_body(&assets, encryption.garbage_dwords).expect("could not write the game");
    let original = original_body(&mut exe);

    println!("{path}\noriginal body {} bytes, ours {} bytes\n", original.len(), ours.len());
    head_report(&mut std::fs::read(&path).expect("could not read the executable"), &assets);
    let differing = report(&walk(&original).expect("walking the original"), &walk(&ours).expect("walking ours"));
    println!(
        "\n{differing} section(s) differ. The head and tail garbage are expected to; anything else is a writer bug."
    );
}
