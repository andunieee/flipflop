//! The app's palette (sky blue, white cards, deep navy text, light and dark) and sizes (a little larger
//! on phones).

use freya::prelude::*;

#[derive(Clone, Copy, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window_bg: Color,
    pub sidebar_bg: Color,
    pub card_bg: Color,
    /// Card borders that tell peers (teal) from groups (blue).
    pub peer_border: Color,
    pub group_border: Color,
    pub inset_bg: Color,
    pub border: Color,
    pub border_strong: Color,

    pub text_strong: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_faint: Color,

    pub accent: Color,
    pub accent_hover: Color,
    pub accent_soft: Color,
    pub accent_text: Color,

    pub online: Color,
    pub offline: Color,
    pub warn: Color,
    pub warn_soft: Color,
    pub info: Color,
    pub info_soft: Color,
    pub error: Color,
    pub error_soft: Color,
    pub success: Color,
    pub success_soft: Color,

    pub chip_bg: Color,
    pub chip_bg_hover: Color,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let c = |light: u32, darkv: u32| Color::new(if dark { darkv } else { light });
        Self {
            dark,
            window_bg: c(0xFFf4faff, 0xFF0e1824),
            sidebar_bg: c(0xFFe6f3fd, 0xFF0a131c),
            card_bg: c(0xFFffffff, 0xFF15222f),
            peer_border: c(0xFF2bb38a, 0xFF34c49a),
            group_border: c(0xFF3d8fe0, 0xFF4f9fea),
            inset_bg: c(0xFFf2f8fd, 0xFF111c28),
            border: c(0xFFd6e7f5, 0xFF223446),
            border_strong: c(0xFFb8d4ec, 0xFF2e4660),

            text_strong: c(0xFF10304a, 0xFFe8f2fb),
            text: c(0xFF2c4a63, 0xFFc2d3e2),
            text_muted: c(0xFF66819a, 0xFF8aa1b6),
            text_faint: c(0xFF95acc0, 0xFF5d7489),

            accent: Color::new(0xFF1e8cf0),
            accent_hover: Color::new(0xFF3a9ff5),
            accent_soft: c(0xFFdbeeff, 0xFF12324f),
            accent_text: c(0xFF1366b8, 0xFF6db8ff),

            online: c(0xFF22b55a, 0xFF2fcb68),
            offline: c(0xFFc2d2df, 0xFF3a4d60),
            warn: c(0xFFe08a00, 0xFFf0a32a),
            warn_soft: c(0xFFfff1d6, 0xFF3a2b0e),
            info: c(0xFF0b8fd6, 0xFF3fb2f0),
            info_soft: c(0xFFdcf2fd, 0xFF0f3047),
            error: c(0xFFe5484d, 0xFFf2676b),
            error_soft: c(0xFFffe5e5, 0xFF3b1a1e),
            success: c(0xFF179c4a, 0xFF3acb70),
            success_soft: c(0xFFdcf6e5, 0xFF123423),

            chip_bg: c(0xFFe8f2fb, 0xFF1c2b3b),
            chip_bg_hover: c(0xFFd9e9f7, 0xFF243649),
        }
    }

    /// Freya's own components (inputs, switches, menus) in the same colours.
    pub fn freya_theme(&self) -> Theme {
        let mut theme = if self.dark { dark_theme() } else { light_theme() };
        theme.name = if self.dark { "dark" } else { "light" };
        let base = if self.dark { DARK_COLORS } else { LIGHT_COLORS };
        theme.colors = ColorsSheet {
            primary: self.accent,
            secondary: self.accent_hover,
            tertiary: self.accent_text,
            success: self.success,
            warning: self.warn,
            error: self.error,
            info: self.info,
            background: self.window_bg,
            surface_primary: self.chip_bg_hover,
            surface_secondary: self.card_bg,
            surface_tertiary: self.inset_bg,
            border: self.border_strong,
            border_focus: self.accent,
            text_primary: self.text_strong,
            text_secondary: self.text_muted,
            text_placeholder: self.text_faint,
            text_highlight: self.accent_text,
            ..base
        };
        // A white thumb on the accent (on) or a grey track (off).
        theme.set(
            "switch",
            SwitchColorsThemePreference {
                background: Preference::Specific(self.offline),
                thumb_background: Preference::Specific(Color::WHITE),
                toggled_background: Preference::Specific(self.accent),
                toggled_thumb_background: Preference::Specific(Color::WHITE),
                focus_border_fill: Preference::Specific(self.accent_text),
            },
        );
        theme
    }
}

/// Sizes; the phone layout is a little larger.
#[derive(Clone, Copy, PartialEq)]
pub struct Dim {
    pub touch: bool,
    /// A narrow window or screen: tighter buttons.
    pub narrow: bool,
}

impl Dim {
    pub const RADIUS_SM: f32 = 6.;
    pub const RADIUS: f32 = 10.;
    pub const RADIUS_LG: f32 = 14.;

    pub const SPACE1: f32 = 4.;
    pub const SPACE2: f32 = 8.;
    pub const SPACE3: f32 = 12.;
    pub const SPACE5: f32 = 24.;

    fn pick(&self, touch: f32, desktop: f32) -> f32 {
        if self.touch { touch } else { desktop }
    }

    pub fn font_xs(&self) -> f32 {
        self.pick(12., 11.)
    }
    pub fn font_sm(&self) -> f32 {
        self.pick(13., 12.)
    }
    pub fn font_md(&self) -> f32 {
        self.pick(15., 13.)
    }
    pub fn font_lg(&self) -> f32 {
        self.pick(17., 15.)
    }
    pub fn font_xl(&self) -> f32 {
        self.pick(20., 22.)
    }
    pub fn control_height(&self) -> f32 {
        self.pick(44., 32.)
    }
    pub fn page_padding(&self) -> f32 {
        self.pick(16., 28.)
    }
    pub fn card_padding(&self) -> f32 {
        self.pick(14., 16.)
    }
    /// Side padding of a labelled button; tighter on narrow windows.
    pub fn button_padding(&self) -> f32 {
        if self.narrow { 8. } else { 14. }
    }
}

/// The phone layout: single pane, bottom navigation, larger controls. On Android, and on the desktop
/// with `FLIPFLOP_PHONE_LAYOUT=1` (to try it without a phone).
pub fn touch() -> bool {
    static TOUCH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TOUCH.get_or_init(|| {
        cfg!(target_os = "android") || std::env::var_os("FLIPFLOP_PHONE_LAYOUT").is_some_and(|v| v == "1")
    })
}

/// The look in use, provided at the root.
#[derive(Clone, Copy, PartialEq)]
pub struct Look {
    pub palette: Palette,
    pub dim: Dim,
}

/// The palette and sizes of the look in use (subscribes the component calling it).
pub fn look() -> (Palette, Dim) {
    let look = consume_context::<State<Look>>();
    let look = *look.read();
    (look.palette, look.dim)
}
