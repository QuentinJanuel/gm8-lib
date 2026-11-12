//! Reassembles an executable around its *own* game data, to separate the container from the body.
//!
//! Everything the writer produces is used -- the swap-table encryption, the 8.1 xor layer, the
//! header splice -- but the body is copied out of the original verbatim. A file built this way that
//! the runner rejects means the container is wrong; one it accepts means the container is right and
//! [`gm8exe::writer::gamedata_body`] is where to look. `bodydiff` cannot answer that question: it
//! compares what this crate reads against what it writes, and both can be wrong together.
//!
//! `IDENTITY=1` goes further and reproduces the file exactly -- the original's own swap table, its
//! own padding, its own body -- and diffs the result against the bytes on disk. That is the only
//! check here with an oracle outside this crate, and it passes on both an 8.0 and an 8.1 game.
//!
//! `FLIP=<n>` then corrupts one byte of the body, negative counting back from its end, into the
//! random padding every real game leaves there and nothing ever reads. This is what established
//! that **GameMaker 8.1 and 8.2 check their game data and 8.0 does not**: with every length and
//! offset left alone, `IDENTITY=1 FLIP=-100` produced an 8.1 game that quit without a word and an
//! 8.0 game that ran. The writer disarms that check now (see `writer::disarm_gm81_data_check`), and
//! `DISARM=1` puts the same patch into a build here, which is how the disarm was shown to be a
//! no-op on a file that was not otherwise touched.
//!
//! `TABLE=orig` and `PADDING=orig` isolate the two things the container decides for itself. Both
//! are how it was found that a GameMaker 8.2 runner refuses a game re-encrypted under a swap table
//! of our own -- the writer now reuses the file's own, via `writer::original_encryption`.
//!
//! Usage: rebuild <exe> <out> [leading-garbage-dwords]
//!   IDENTITY=1  reproduce the original exactly and diff against it
//!   FLIP=<n>    corrupt body byte n (negative counts from the end) before encrypting
//!   TABLE=orig  encrypt with the original's swap table rather than the writer's
//!   PADDING=orig  keep the original's garbage around the swap table rather than writing none
use byteorder::{LE, ReadBytesExt};
use gm8exe::{
    gamedata::{gm80, gm81},
    writer,
};
use std::io::{Cursor, Seek, SeekFrom};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: rebuild <exe> <out> [garbage]");
    let out_path = args.next().expect("out");
    let regarbage: Option<usize> = args.next().map(|n| n.parse().expect("garbage"));

    let original = std::fs::read(&path).expect("read");
    let layout = writer::find_layout(&original).expect("not a plain GameMaker 8 executable");

    let mut plain = original.clone();
    if let writer::Layout::Gm81 { header } = &layout {
        gm81::apply(&mut plain, header);
    }

    // Walk the plaintext head to the encrypted region, then decrypt it in place.
    let mut c = Cursor::new(&mut plain[..]);
    c.set_position(layout.start() as u64);
    for _ in 0..3 {
        let len = c.read_u32::<LE>().unwrap() as i64;
        c.seek(SeekFrom::Current(len)).unwrap();
    }
    let region = c.position() as usize;
    c.set_position(region as u64);
    gm80::decrypt(&mut c, None::<fn(&str)>).unwrap();

    c.set_position(region as u64);
    let g1 = c.read_u32::<LE>().unwrap() as i64;
    let g2 = c.read_u32::<LE>().unwrap() as i64;
    c.seek(SeekFrom::Current(g1 * 4 + 256 + g2 * 4)).unwrap();
    let body_len = c.read_u32::<LE>().unwrap() as usize;
    let body_start = c.position() as usize;
    let mut body = plain[body_start..body_start + body_len].to_vec();
    println!("region at {region}, body {body_len} bytes, original leading garbage {}", {
        u32::from_le_bytes([body[0], body[1], body[2], body[3]])
    });

    // Optionally re-cut the leading garbage, which is the one part of the body the writer decides
    // for itself. Only shrinking is possible here: the bytes have to come from somewhere.
    if let Some(want) = regarbage {
        let had = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
        assert!(want <= had, "can only trim the original's {had} dwords");
        body.drain(4..4 + (had - want) * 4);
        body[..4].copy_from_slice(&(want as u32).to_le_bytes());
        println!("trimmed leading garbage to {want} dwords, body now {} bytes", body.len());
    }

    // Identity mode: re-encrypt the original's own body with the original's own swap table and its
    // own garbage, then compare against the file on disk. Anything but "identical" is a bug in the
    // container -- the cipher, the splice, or the 8.1 xor -- with no body writer involved at all.
    if std::env::var("IDENTITY").is_ok() {
        let table_at = region + 8 + g1 as usize * 4;
        let mut table = [0u8; 256];
        table.copy_from_slice(&plain[table_at..table_at + 256]);
        let mut encrypted = plain[body_start..body_start + body_len].to_vec();
        // FLIP=<n> corrupts one byte of the body's leading garbage -- bytes nothing reads -- while
        // leaving every length, offset and structure alone. A runner that refuses the result is
        // checking the file's integrity rather than parsing it.
        if let Ok(at) = std::env::var("FLIP") {
            // Negative counts back from the end of the body, where every real game leaves a run of
            // random padding the reader stops before -- the least readable bytes in the file.
            let at: i64 = at.parse().expect("FLIP");
            let at = if at < 0 { (body_len as i64 + at) as usize } else { at as usize };
            encrypted[at] ^= 0xFF;
            println!("flipped body byte {at}");
        }
        writer::encrypt_gamedata(&mut encrypted, &table, (body_len as u32 >> 24) as u8);
        let mut out = plain[..body_start].to_vec();
        out.extend_from_slice(&encrypted);
        if let writer::Layout::Gm81 { header } = &layout {
            gm81::apply(&mut out, header);
        }
        if std::env::var("DISARM").is_ok() {
            writer::disarm_gm81_data_check(&mut out).expect("disarm");
            println!("identity: plus the data-check patch");
        }
        println!("identity: {} vs {} bytes", out.len(), original.len());
        match out.iter().zip(&original).position(|(a, b)| a != b) {
            None if out.len() == original.len() => println!("identity: IDENTICAL"),
            None => println!("identity: one is a prefix of the other"),
            Some(at) => {
                println!("identity: first difference at {at} (region {region}, body {body_start})");
                println!("  original {:02X?}", &original[at..at + 24]);
                println!("  ours     {:02X?}", &out[at..at + 24]);
            },
        }
        std::fs::write(&out_path, &out).expect("write");
        return
    }

    // Two knobs, because the difference between this and identity mode is exactly two things: the
    // swap table the body is encrypted with, and the padding that hides it.
    let table = if std::env::var("TABLE").as_deref() == Ok("orig") {
        let table_at = region + 8 + g1 as usize * 4;
        let mut table = [0u8; 256];
        table.copy_from_slice(&plain[table_at..table_at + 256]);
        println!("using the original's swap table");
        table
    } else {
        writer::default_swap_table()
    };
    let keep_padding = std::env::var("PADDING").as_deref() == Ok("orig");
    if keep_padding {
        println!("keeping the original's padding: {g1} + {g2} dwords");
    }

    let mut out = plain[..region].to_vec();
    if keep_padding {
        out.extend_from_slice(&(g1 as u32).to_le_bytes());
        out.extend_from_slice(&(g2 as u32).to_le_bytes());
        out.extend_from_slice(&plain[region + 8..region + 8 + g1 as usize * 4]);
        out.extend_from_slice(&table);
        let after_table = region + 8 + g1 as usize * 4 + 256;
        out.extend_from_slice(&plain[after_table..after_table + g2 as usize * 4]);
        let mut encrypted = body.clone();
        writer::encrypt_gamedata(&mut encrypted, &table, (body.len() as u32 >> 24) as u8);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&encrypted);
    } else {
        writer::write_encrypted_gamedata(&mut out, &body, &table).expect("encrypt");
    }
    if let writer::Layout::Gm81 { header } = &layout {
        gm81::apply(&mut out, header);
    }
    if let writer::Layout::Gm81 { .. } = &layout {
        // Same as `to_exe` does, or the runner's own check refuses what we just built.
        writer::disarm_gm81_data_check(&mut out).expect("disarm");
    }
    std::fs::write(&out_path, &out).expect("write");
    println!("wrote {out_path}: {} bytes (original {})", out.len(), original.len());
}
