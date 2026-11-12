use crate::{asset::PascalString, colour::Colour};

/// The Settings header for a GM8 game
pub struct Settings {
    /// Start in full-screen mode
    pub fullscreen: bool,

    /// Scaling
    ///
    /// Fixed scale, in %.
    /// If it's negative (usually `-1`), Keep aspect ratio.
    /// Otherwise if it's `0`, Full scale.
    pub scaling: i32,

    /// Interpolate colours between pixels
    pub interpolate_pixels: bool,

    /// Colour outside the room region (RGBA)
    pub clear_colour: u32,

    /// Allow the player to resize the game window
    pub allow_resize: bool,

    /// Let the game window always stay on top
    pub window_on_top: bool,

    /// Don't draw a border in windowed mode
    pub dont_draw_border: bool,

    /// Don't show the buttons in the window captions
    pub dont_show_buttons: bool,

    /// Display the cursor
    pub display_cursor: bool,

    /// Freeze the game window when the window loses focus
    pub freeze_on_lose_focus: bool,

    /// Disable screensavers and power saving actions
    pub disable_screensaver: bool,

    /// Force Direct3D software vertex processing (GM 8.0 behaviour)
    pub force_cpu_render: bool,

    /// Set the resolution of the screen
    pub set_resolution: bool,

    /// Sub-var of `set_resolution` - Color Depth
    ///
    /// 0 - No Change
    ///
    /// 1 - 16-Bit
    ///
    /// 2 - 32-Bit
    pub colour_depth: u32,

    /// Sub-var of `set_resolution` - Resolution
    ///
    /// 0 - No change
    ///
    /// 1 - 320x240
    ///
    /// 2 - 640x480
    ///
    /// 3 - 800x600
    ///
    /// 4 - 1024x768
    ///
    /// 5 - 1280x1024
    ///
    /// 6 - 1600x1200
    pub resolution: u32,

    /// Sub-var of `set_resolution` - Frequency
    ///
    /// 0 - No Change
    ///
    /// 1 - 60Hz
    ///
    /// 2 - 70Hz
    ///
    /// 3 - 85Hz
    ///
    /// 4 - 100Hz
    ///
    /// 5 - 120Hz
    pub frequency: u32,

    /// Use synchronization to avoid tearing
    pub vsync: bool,

    /// Let <Esc> end the game
    pub esc_close_game: bool,

    /// Treat the close button as the <Esc> key
    pub treat_close_as_esc: bool,

    /// Let <F1> show the game information
    pub f1_help_menu: bool,

    /// Let <F4> switch between screen modes
    pub f4_fullscreen_toggle: bool,

    /// Let <F5> save the game and <F6> load a game
    pub f5_save_f6_load: bool,

    /// Let <F9> take a screenshot of the game
    pub f9_screenshot: bool,

    /// Game Process Priority
    ///
    /// 0 - Normal
    ///
    /// 1 - High
    ///
    /// 2 - Highest
    ///
    pub priority: u32,

    /// Show your own image while loading (data)
    pub custom_load_image: Option<Box<[u8]>>,

    /// Sub-value of `custom_load_image`:
    /// Make image partially translucent
    pub transparent: bool,

    /// Sub-value of `custom_load_image` + `transparent`
    ///
    /// Make translucent with alpha value: x
    pub translucency: u32,

    /// 0 - No loading progress bar
    ///
    /// 1 - Default loading progress bar
    ///
    /// 2 - Own loading progress bar
    pub loading_bar: u32,

    /// Loading bar - (Custom) Back Image
    pub backdata: Option<Box<[u8]>>,

    /// Loading bar - (Custom) Front Image
    pub frontdata: Option<Box<[u8]>>,

    /// Scale progress bar image
    pub scale_progress_bar: bool,

    /// Display error messages
    pub show_error_messages: bool,

    /// Write error messages to file game_errors.log
    pub log_errors: bool,

    /// Abort on all error messages
    pub always_abort: bool,

    /// Treat uninitialized variables as value 0
    pub zero_uninitialized_vars: bool,

    /// Throw an error when arguments aren't initialized correctly
    pub error_on_uninitialized_args: bool,

    /// The WebGL flag, which nothing here reads -- and, by its presence, whether the settings chunk
    /// carries the pair of dwords it and `swap_creation_events` make up at all.
    ///
    /// `None` means the chunk simply stops before them, which is what builds before 8.1.141 wrote.
    /// The difference has to survive a round trip: a settings chunk eight bytes longer than the one
    /// the game shipped is a game the runner refuses to load. Kamilia 3 is one of these.
    pub webgl: Option<u32>,

    /// Run create events before instance creation code (not available in base 8.1).
    ///
    /// Only meaningful when [`Settings::webgl`] is `Some`: the two are written as a pair or not at
    /// all, and the reader reports `false` for a chunk that carries neither.
    pub swap_creation_events: bool,
}

/// The help dialog box associated with a GM8 game
#[derive(Debug)]
pub struct GameHelpDialog {
    pub bg_colour: Colour,
    pub new_window: bool,
    pub caption: PascalString,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub border: bool,
    pub resizable: bool,
    pub window_on_top: bool,
    pub freeze_game: bool,
    pub info: PascalString,
}

use crate::{GameVersion, asset::WritePascalString};
use byteorder::{LE, WriteBytesExt};
use std::io;

/// Writes an optional data block the way the reader's `read_data_maybe` expects to find one: a
/// presence flag, and only then a length and the bytes.
fn write_data_maybe(writer: &mut impl io::Write, data: &Option<Box<[u8]>>) -> io::Result<()> {
    match data {
        Some(bytes) => {
            writer.write_u32::<LE>(1)?;
            writer.write_u32::<LE>(bytes.len() as u32)?;
            writer.write_all(bytes)?;
        },
        None => writer.write_u32::<LE>(0)?,
    }
    Ok(())
}

impl Settings {
    /// Writes the settings chunk, in the order the reader reads it.
    ///
    /// Two fields are packed into one dword each, and how depends on the version: 8.0 spends a whole
    /// dword where 8.1 uses bit flags. Writing 8.1's packing into an 8.0 game would turn
    /// `force_cpu_render` on and vsync off, so the version is a parameter rather than an assumption.
    pub fn write(&self, writer: &mut impl io::Write, version: GameVersion) -> io::Result<()> {
        writer.write_u32::<LE>(self.fullscreen as u32)?;
        writer.write_u32::<LE>(self.interpolate_pixels as u32)?;
        writer.write_u32::<LE>(self.dont_draw_border as u32)?;
        writer.write_u32::<LE>(self.display_cursor as u32)?;
        writer.write_i32::<LE>(self.scaling)?;
        writer.write_u32::<LE>(self.allow_resize as u32)?;
        writer.write_u32::<LE>(self.window_on_top as u32)?;
        writer.write_u32::<LE>(self.clear_colour)?;
        writer.write_u32::<LE>(self.set_resolution as u32)?;
        writer.write_u32::<LE>(self.colour_depth)?;
        writer.write_u32::<LE>(self.resolution)?;
        writer.write_u32::<LE>(self.frequency)?;
        writer.write_u32::<LE>(self.dont_show_buttons as u32)?;
        writer.write_u32::<LE>(match version {
            // 8.0 has no force_cpu_render bit; the runner behaves as though it were always on, which
            // is why the reader hard-codes it true and there is nothing to write back here.
            GameVersion::GameMaker8_0 => self.vsync as u32,
            GameVersion::GameMaker8_1 => (self.vsync as u32) | ((self.force_cpu_render as u32) << 7),
        })?;
        writer.write_u32::<LE>(self.disable_screensaver as u32)?;
        writer.write_u32::<LE>(self.f4_fullscreen_toggle as u32)?;
        writer.write_u32::<LE>(self.f1_help_menu as u32)?;
        writer.write_u32::<LE>(self.esc_close_game as u32)?;
        writer.write_u32::<LE>(self.f5_save_f6_load as u32)?;
        writer.write_u32::<LE>(self.f9_screenshot as u32)?;
        writer.write_u32::<LE>(self.treat_close_as_esc as u32)?;
        writer.write_u32::<LE>(self.priority)?;
        writer.write_u32::<LE>(self.freeze_on_lose_focus as u32)?;
        writer.write_u32::<LE>(self.loading_bar)?;
        // The two bar images are only present at all when a custom bar was chosen, and the reader
        // does not look for them otherwise -- writing them anyway would shift everything after.
        if self.loading_bar != 0 {
            write_data_maybe(writer, &self.backdata)?;
            write_data_maybe(writer, &self.frontdata)?;
        }
        write_data_maybe(writer, &self.custom_load_image)?;
        writer.write_u32::<LE>(self.transparent as u32)?;
        writer.write_u32::<LE>(self.translucency)?;
        writer.write_u32::<LE>(self.scale_progress_bar as u32)?;
        writer.write_u32::<LE>(self.show_error_messages as u32)?;
        writer.write_u32::<LE>(self.log_errors as u32)?;
        writer.write_u32::<LE>(self.always_abort as u32)?;
        writer.write_u32::<LE>(match version {
            GameVersion::GameMaker8_0 => self.zero_uninitialized_vars as u32,
            GameVersion::GameMaker8_1 => {
                (self.zero_uninitialized_vars as u32) | ((self.error_on_uninitialized_args as u32) << 1)
            },
        })?;
        // Written as a pair, or not at all. A chunk that never carried them is eight bytes shorter,
        // and handing the runner the longer one is enough on its own to make the game not load.
        if let Some(webgl) = self.webgl {
            writer.write_u32::<LE>(webgl)?;
            writer.write_u32::<LE>(self.swap_creation_events as u32)?;
        }
        Ok(())
    }
}

impl GameHelpDialog {
    /// Writes the help dialog's inner block. The caller supplies the compression and length around
    /// it, exactly as the reader inflates a block and then reads these fields out of it.
    pub fn write(&self, writer: &mut impl io::Write) -> io::Result<()> {
        writer.write_u32::<LE>(self.bg_colour.into())?;
        writer.write_u32::<LE>(self.new_window as u32)?;
        writer.write_pas_string(&self.caption)?;
        writer.write_i32::<LE>(self.left)?;
        writer.write_i32::<LE>(self.top)?;
        writer.write_u32::<LE>(self.width)?;
        writer.write_u32::<LE>(self.height)?;
        writer.write_u32::<LE>(self.border as u32)?;
        writer.write_u32::<LE>(self.resizable as u32)?;
        writer.write_u32::<LE>(self.window_on_top as u32)?;
        writer.write_u32::<LE>(self.freeze_game as u32)?;
        writer.write_pas_string(&self.info)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field zeroed, so a test can say which one it is about.
    fn blank() -> Settings {
        Settings {
            fullscreen: false,
            scaling: 0,
            interpolate_pixels: false,
            clear_colour: 0,
            allow_resize: false,
            window_on_top: false,
            dont_draw_border: false,
            dont_show_buttons: false,
            display_cursor: false,
            freeze_on_lose_focus: false,
            disable_screensaver: false,
            force_cpu_render: false,
            set_resolution: false,
            colour_depth: 0,
            resolution: 0,
            frequency: 0,
            vsync: false,
            esc_close_game: false,
            treat_close_as_esc: false,
            f1_help_menu: false,
            f4_fullscreen_toggle: false,
            f5_save_f6_load: false,
            f9_screenshot: false,
            priority: 0,
            custom_load_image: None,
            transparent: false,
            translucency: 0,
            loading_bar: 0,
            backdata: None,
            frontdata: None,
            scale_progress_bar: false,
            show_error_messages: false,
            log_errors: false,
            always_abort: false,
            zero_uninitialized_vars: false,
            error_on_uninitialized_args: false,
            webgl: None,
            swap_creation_events: false,
        }
    }

    fn written(settings: &Settings) -> Vec<u8> {
        let mut out = Vec::new();
        settings.write(&mut out, GameVersion::GameMaker8_1).expect("write");
        out
    }

    /// The bug that cost a whole session: a settings chunk that never carried the last two dwords
    /// was written eight bytes longer than it came in, and GameMaker refused the game -- with every
    /// other section of the file reproduced byte for byte, so nothing pointed at this.
    #[test]
    fn a_chunk_without_the_trailing_pair_is_written_without_it() {
        let short = written(&blank());
        let long = written(&Settings { webgl: Some(0), ..blank() });
        assert_eq!(long.len(), short.len() + 8, "the pair is eight bytes, present or absent");
    }

    /// The WebGL dword is carried across rather than zeroed. Nothing here reads it, which is exactly
    /// the reasoning that has produced four writer bugs already.
    #[test]
    fn the_webgl_dword_is_written_back_as_it_was_read() {
        let out = written(&Settings { webgl: Some(0xDEAD_BEEF), swap_creation_events: true, ..blank() });
        let tail = &out[out.len() - 8..];
        assert_eq!(u32::from_le_bytes(tail[..4].try_into().unwrap()), 0xDEAD_BEEF);
        assert_eq!(u32::from_le_bytes(tail[4..].try_into().unwrap()), 1);
    }
}
