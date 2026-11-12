# Vendored binaries

## `http_dll8`

A pre-serialised GameMaker 8 *extension record* wrapping **Http Dll 2.3** by Maarten Baert
(258,560-byte PE32 DLL, 129 functions, 14 constants). Its documentation is vendored at `http-dll-2/`.

It is here because GameMaker 8 has no sockets and no buffers of its own -- zero `network_*` and
zero `buffer_*` functions -- so a mod channel in a patched GameMaker 8 game has to come from an
extension. This blob is spliced into a game's extension table by `mods::install_extension`, which
parses it with our own `gm8exe` reader rather than trusting its bytes blindly. Its documentation is
in `http-dll-2/`.

Taken from I Wanna Play Online's `modder` repository (`lib/http_dll8`), which ships it under MIT.
The DLL inside it is Http Dll 2.3 by Maarten Baert, distributed under the LGPL (see `NOTICE`), and
redistributed inside every game `mods::patch_exe` produces.
