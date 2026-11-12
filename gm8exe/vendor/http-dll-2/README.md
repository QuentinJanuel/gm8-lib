# Http Dll 2.3 — vendored documentation

Third-party documentation, saved verbatim on 2026-09-09 from
<https://www.maartenbaert.be/game-maker-dlls/http-dll-2/> (author: Maarten Baert).

Kept here because the GameMaker 8 mod channel is built on this DLL, and a website is not a
dependency you want between a maintainer and the wire format of a shipped protocol.

| file | what it covers |
|---|---|
| `index.html` / `.txt` | overview, installation, changelog, FAQ |
| `sockets.html` / `.txt` | TCP sockets — **the mod channel lives here** |
| `udp-sockets.html` / `.txt` | UDP sockets (not used by the channel) |
| `buffers.html` / `.txt` | the buffer API, and the `intv`/`uintv` encodings |
| `http-requests.html` / `.txt` | HTTP client (unused here) |
| `md5-and-sha-1.html` / `.txt` | hashing (unused here) |

The `.txt` files are tag-stripped extractions of the `.html` beside them, generated for grepping.
The HTML is the source of truth; regenerate the text rather than editing it.

## The parts that matter here

**The socket API is general-purpose**, which is what makes it usable as a mod transport rather than
only for a game's own networking:

```
socket_create/destroy/exists/reset/get_state
socket_connect/shut_down/get_peer_address
socket_update_read/update_write          -- pump, called every step
socket_read_data/write_data              -- raw bytes
socket_read_message/write_message        -- length-prefixed framing
socket_read_message_delimiter/write_message_delimiter   -- delimiter framing
socket_get_read_data_length/get_write_data_length
```

**Framing.** `socket_write_message` sends "the size of the message (as uintv), followed by the
actual message data". `uintv` is a little-endian prefix code: the low bits of the first byte say
whether 1, 2, 3 or 4 bytes are used, the rest carry the value, with offsets
`[0, 0x80, 0x4080, 0x204080]`. IWPO's server implements exactly this in `readUIntV` /
`insertUIntV`, which is a working reference for the host side.

There is also `socket_write_message_delimiter(id, buffer_id, delimiter)` with a one-character
delimiter — the docs' own example uses `chr(10)`. Tempting, because a NUL-delimited channel is a natural fit, but the delimiter crosses the extension boundary as a C string, so `chr(0)`
is the one value that probably cannot survive it. Length-prefixed framing is also the only option
that stays correct when a mod sends a message containing the delimiter, and mod messages are
arbitrary strings.

**Receiving is polled, not event-driven.** `socket_update_read` then `socket_read_message` in a
Step event; GameMaker 8 has no async networking event.

## Provenance

Not part of this repository's own work. Http Dll 2.3 is by Maarten Baert and is distributed under the
GNU Lesser General Public License (see the licence section of `index.txt`). The DLL and
`lib/http_dll8`, a pre-serialised GameMaker 8 extension record that embeds it, were taken from
I Wanna Play Online's `modder` repository. It is redistributed inside every patched game
`mods::patch_exe` produces, with credit to Maarten Baert.
