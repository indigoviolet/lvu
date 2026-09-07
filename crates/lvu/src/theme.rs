//! Immutable semantic color palettes. Selection and storage live with the host;
//! this module has no process-global state or environment access. The host
//! reads the environment and hands the answer in as a [`ColorDepth`].

use ratatui::style::Color;

/// WCAG contrast floor used for data-driven identity colors on concrete themes.
pub const MIN_IDENTITY_CONTRAST: f64 = 3.0;

/// The six channel levels of the xterm 6x6x6 color cube, in order.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
/// First index of the 6x6x6 cube; 0..16 are the system colors.
const CUBE_BASE: u8 = 16;

/// How many colors the attached terminal can actually show.
///
/// Data-driven identity colors are computed from a hash, so they can land
/// anywhere in the 24-bit space. A terminal without truecolor support does not
/// reject those sequences; it silently approximates or drops them, which is how
/// two different values end up looking identical and how a hashed color ends up
/// unreadable on the theme background. Choosing the color from the palette the
/// terminal really has keeps both properties under lvu's control instead of the
/// emulator's.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ColorDepth {
    /// 24-bit `Color::Rgb`, emitted as `38;2;R;G;B`.
    #[default]
    TrueColor,
    /// The xterm 256-color cube, emitted as `38;5;N`.
    Indexed256,
}

impl ColorDepth {
    /// The depth a `COLORTERM` value implies.
    ///
    /// `NO_COLOR` is deliberately not consulted: crossterm already honours it
    /// at the point sequences are emitted, so suppressing color a second time
    /// here would only make lvu's own colors disagree with its output.
    pub fn from_colorterm(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("truecolor" | "24bit") => Self::TrueColor,
            _ => Self::Indexed256,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ThemeId {
    #[default]
    Terminal,
    LoveDark,
    LoveLight,
    Dracula,
    Nord,
    GruvboxDark,
}

impl ThemeId {
    pub const ALL: [Self; 6] = [
        Self::Terminal,
        Self::LoveDark,
        Self::LoveLight,
        Self::Dracula,
        Self::Nord,
        Self::GruvboxDark,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::LoveDark => "love-dark",
            Self::LoveLight => "love-light",
            Self::Dracula => "dracula",
            Self::Nord => "nord",
            Self::GruvboxDark => "gruvbox-dark",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::LoveDark => "Love Dark",
            Self::LoveLight => "Love Light",
            Self::Dracula => "Dracula",
            Self::Nord => "Nord",
            Self::GruvboxDark => "Gruvbox Dark",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == value)
    }

    pub const fn theme(self) -> Theme {
        match self {
            Self::Terminal => Theme::TERMINAL,
            Self::LoveDark => Theme::LOVE_DARK,
            Self::LoveLight => Theme::LOVE_LIGHT,
            Self::Dracula => Theme::DRACULA,
            Self::Nord => Theme::NORD,
            Self::GruvboxDark => Theme::GRUVBOX_DARK,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeverityColors {
    pub fatal: Color,
    pub error: Color,
    pub warn: Color,
    pub info: Color,
    pub debug: Color,
    pub trace: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeartColors {
    pub primary: Color,
    pub soft: Color,
    pub deep: Color,
    pub error: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JsonColors {
    pub string: Color,
    pub number: Color,
    pub boolean: Color,
    pub null: Color,
    pub punctuation: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Theme {
    pub id: ThemeId,
    /// Set by the host from the environment; the palette constants are written
    /// in 24-bit and downgraded on the way out, never at definition time.
    pub depth: ColorDepth,
    pub base_fg: Color,
    pub base_bg: Color,
    pub dialog_bg: Color,
    pub input_bg: Color,
    pub input_fg: Color,
    pub focused_input_border: Color,
    pub cursor: Color,
    pub muted: Color,
    pub border: Color,
    pub active_border: Color,
    pub accent: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub severity: SeverityColors,
    pub categorical: [Color; 5],
    pub identity_saturation: u8,
    pub identity_lightness: u8,
    pub json: JsonColors,
    pub heart: HeartColors,
}

impl Theme {
    pub const TERMINAL: Self = Self {
        id: ThemeId::Terminal,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Reset,
        base_bg: Color::Reset,
        dialog_bg: Color::Reset,
        input_bg: Color::DarkGray,
        input_fg: Color::White,
        focused_input_border: Color::Cyan,
        cursor: Color::Yellow,
        muted: Color::DarkGray,
        border: Color::DarkGray,
        active_border: Color::Yellow,
        accent: Color::Cyan,
        selection_fg: Color::Black,
        selection_bg: Color::Yellow,
        severity: SeverityColors {
            fatal: Color::Red,
            error: Color::Red,
            warn: Color::Yellow,
            info: Color::Green,
            debug: Color::Blue,
            trace: Color::DarkGray,
        },
        categorical: [
            Color::Cyan,
            Color::Magenta,
            Color::Blue,
            Color::Green,
            Color::Yellow,
        ],
        identity_saturation: 68,
        identity_lightness: 58,
        json: JsonColors {
            string: Color::Green,
            number: Color::Blue,
            boolean: Color::Yellow,
            null: Color::Magenta,
            punctuation: Color::Gray,
        },
        heart: HeartColors {
            primary: Color::Rgb(255, 111, 97),
            soft: Color::Rgb(238, 137, 124),
            deep: Color::Rgb(211, 82, 76),
            error: Color::Red,
        },
    };

    pub const LOVE_DARK: Self = Self {
        id: ThemeId::LoveDark,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Rgb(244, 231, 234),
        base_bg: Color::Rgb(29, 22, 29),
        dialog_bg: Color::Rgb(38, 27, 36),
        // #46303f — a real tone against dialog_bg (1.39:1); see dialog-system.md §6.1.
        input_bg: Color::Rgb(70, 48, 63),
        input_fg: Color::Rgb(255, 241, 242),
        focused_input_border: Color::Rgb(255, 158, 143),
        cursor: Color::Rgb(255, 220, 185),
        muted: Color::Rgb(174, 145, 154),
        border: Color::Rgb(112, 82, 94),
        active_border: Color::Rgb(255, 158, 143),
        accent: Color::Rgb(255, 126, 112),
        selection_fg: Color::Rgb(35, 20, 25),
        selection_bg: Color::Rgb(255, 167, 151),
        severity: SeverityColors {
            fatal: Color::Rgb(255, 94, 105),
            error: Color::Rgb(255, 112, 116),
            warn: Color::Rgb(255, 200, 122),
            info: Color::Rgb(126, 220, 166),
            debug: Color::Rgb(128, 190, 255),
            trace: Color::Rgb(174, 145, 154),
        },
        categorical: [
            Color::Rgb(104, 211, 219),
            Color::Rgb(225, 137, 255),
            Color::Rgb(128, 190, 255),
            Color::Rgb(126, 220, 166),
            Color::Rgb(255, 200, 122),
        ],
        identity_saturation: 72,
        identity_lightness: 68,
        json: JsonColors {
            string: Color::Rgb(126, 220, 166),
            number: Color::Rgb(128, 190, 255),
            boolean: Color::Rgb(255, 200, 122),
            null: Color::Rgb(255, 126, 112),
            punctuation: Color::Rgb(244, 231, 234),
        },
        heart: HeartColors {
            primary: Color::Rgb(255, 111, 97),
            soft: Color::Rgb(238, 151, 143),
            deep: Color::Rgb(214, 77, 82),
            error: Color::Rgb(255, 94, 105),
        },
    };

    pub const LOVE_LIGHT: Self = Self {
        id: ThemeId::LoveLight,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Rgb(58, 43, 48),
        base_bg: Color::Rgb(255, 248, 246),
        dialog_bg: Color::Rgb(250, 237, 234),
        // #e6c8c4 — a real tone against dialog_bg (1.37:1); see dialog-system.md §6.1.
        input_bg: Color::Rgb(230, 200, 196),
        input_fg: Color::Rgb(70, 42, 49),
        focused_input_border: Color::Rgb(184, 67, 66),
        cursor: Color::Rgb(125, 35, 43),
        muted: Color::Rgb(126, 101, 108),
        border: Color::Rgb(202, 169, 174),
        active_border: Color::Rgb(184, 67, 66),
        accent: Color::Rgb(194, 70, 67),
        selection_fg: Color::Rgb(255, 252, 250),
        selection_bg: Color::Rgb(166, 54, 58),
        severity: SeverityColors {
            fatal: Color::Rgb(157, 30, 45),
            error: Color::Rgb(181, 43, 52),
            warn: Color::Rgb(143, 91, 0),
            info: Color::Rgb(34, 112, 70),
            debug: Color::Rgb(45, 91, 160),
            trace: Color::Rgb(126, 101, 108),
        },
        categorical: [
            Color::Rgb(24, 117, 126),
            Color::Rgb(135, 62, 157),
            Color::Rgb(45, 91, 160),
            Color::Rgb(34, 112, 70),
            Color::Rgb(143, 91, 0),
        ],
        identity_saturation: 63,
        identity_lightness: 36,
        json: JsonColors {
            string: Color::Rgb(34, 112, 70),
            number: Color::Rgb(45, 91, 160),
            boolean: Color::Rgb(143, 91, 0),
            null: Color::Rgb(194, 70, 67),
            punctuation: Color::Rgb(58, 43, 48),
        },
        heart: HeartColors {
            primary: Color::Rgb(194, 70, 67),
            soft: Color::Rgb(171, 86, 86),
            deep: Color::Rgb(151, 45, 50),
            error: Color::Rgb(157, 30, 45),
        },
    };

    /// Dracula's canonical palette mapped onto lvu's semantic roles.
    pub const DRACULA: Self = Self {
        id: ThemeId::Dracula,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Rgb(248, 248, 242),
        base_bg: Color::Rgb(40, 42, 54),
        dialog_bg: Color::Rgb(44, 46, 59),
        input_bg: Color::Rgb(68, 71, 90),
        input_fg: Color::Rgb(248, 248, 242),
        focused_input_border: Color::Rgb(139, 233, 253),
        cursor: Color::Rgb(241, 250, 140),
        muted: Color::Rgb(98, 114, 164),
        border: Color::Rgb(68, 71, 90),
        active_border: Color::Rgb(189, 147, 249),
        accent: Color::Rgb(255, 121, 198),
        selection_fg: Color::Rgb(248, 248, 242),
        selection_bg: Color::Rgb(68, 71, 90),
        severity: SeverityColors {
            fatal: Color::Rgb(255, 85, 85),
            error: Color::Rgb(255, 85, 85),
            warn: Color::Rgb(255, 184, 108),
            info: Color::Rgb(80, 250, 123),
            debug: Color::Rgb(139, 233, 253),
            trace: Color::Rgb(98, 114, 164),
        },
        categorical: [
            Color::Rgb(139, 233, 253),
            Color::Rgb(255, 121, 198),
            Color::Rgb(189, 147, 249),
            Color::Rgb(80, 250, 123),
            Color::Rgb(241, 250, 140),
        ],
        identity_saturation: 88,
        identity_lightness: 70,
        json: JsonColors {
            string: Color::Rgb(80, 250, 123),
            number: Color::Rgb(139, 233, 253),
            boolean: Color::Rgb(241, 250, 140),
            null: Color::Rgb(255, 121, 198),
            punctuation: Color::Rgb(248, 248, 242),
        },
        heart: HeartColors {
            primary: Color::Rgb(255, 121, 198),
            soft: Color::Rgb(189, 147, 249),
            deep: Color::Rgb(255, 85, 85),
            error: Color::Rgb(255, 85, 85),
        },
    };

    /// Nord's Polar Night, Snow Storm, Frost, and Aurora palettes.
    pub const NORD: Self = Self {
        id: ThemeId::Nord,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Rgb(216, 222, 233),
        base_bg: Color::Rgb(46, 52, 64),
        dialog_bg: Color::Rgb(52, 59, 72),
        input_bg: Color::Rgb(59, 66, 82),
        input_fg: Color::Rgb(236, 239, 244),
        focused_input_border: Color::Rgb(136, 192, 208),
        cursor: Color::Rgb(235, 203, 139),
        muted: Color::Rgb(129, 161, 193),
        border: Color::Rgb(76, 86, 106),
        active_border: Color::Rgb(136, 192, 208),
        accent: Color::Rgb(136, 192, 208),
        selection_fg: Color::Rgb(236, 239, 244),
        selection_bg: Color::Rgb(67, 76, 94),
        severity: SeverityColors {
            fatal: Color::Rgb(191, 97, 106),
            error: Color::Rgb(191, 97, 106),
            warn: Color::Rgb(235, 203, 139),
            info: Color::Rgb(163, 190, 140),
            debug: Color::Rgb(129, 161, 193),
            trace: Color::Rgb(76, 86, 106),
        },
        categorical: [
            Color::Rgb(136, 192, 208),
            Color::Rgb(180, 142, 173),
            Color::Rgb(129, 161, 193),
            Color::Rgb(163, 190, 140),
            Color::Rgb(235, 203, 139),
        ],
        identity_saturation: 42,
        identity_lightness: 68,
        json: JsonColors {
            string: Color::Rgb(163, 190, 140),
            number: Color::Rgb(129, 161, 193),
            boolean: Color::Rgb(235, 203, 139),
            null: Color::Rgb(180, 142, 173),
            punctuation: Color::Rgb(216, 222, 233),
        },
        heart: HeartColors {
            primary: Color::Rgb(191, 97, 106),
            soft: Color::Rgb(180, 142, 173),
            deep: Color::Rgb(143, 74, 83),
            error: Color::Rgb(191, 97, 106),
        },
    };

    /// Gruvbox's canonical dark background and bright foreground colors.
    pub const GRUVBOX_DARK: Self = Self {
        id: ThemeId::GruvboxDark,
        depth: ColorDepth::TrueColor,
        base_fg: Color::Rgb(235, 219, 178),
        base_bg: Color::Rgb(40, 40, 40),
        dialog_bg: Color::Rgb(50, 48, 47),
        input_bg: Color::Rgb(60, 56, 54),
        input_fg: Color::Rgb(251, 241, 199),
        focused_input_border: Color::Rgb(142, 192, 124),
        cursor: Color::Rgb(250, 189, 47),
        muted: Color::Rgb(146, 131, 116),
        border: Color::Rgb(80, 73, 69),
        active_border: Color::Rgb(254, 128, 25),
        accent: Color::Rgb(254, 128, 25),
        selection_fg: Color::Rgb(40, 40, 40),
        selection_bg: Color::Rgb(250, 189, 47),
        severity: SeverityColors {
            fatal: Color::Rgb(251, 73, 52),
            error: Color::Rgb(251, 73, 52),
            warn: Color::Rgb(250, 189, 47),
            info: Color::Rgb(184, 187, 38),
            debug: Color::Rgb(131, 165, 152),
            trace: Color::Rgb(146, 131, 116),
        },
        categorical: [
            Color::Rgb(142, 192, 124),
            Color::Rgb(211, 134, 155),
            Color::Rgb(131, 165, 152),
            Color::Rgb(184, 187, 38),
            Color::Rgb(250, 189, 47),
        ],
        identity_saturation: 66,
        identity_lightness: 67,
        json: JsonColors {
            string: Color::Rgb(184, 187, 38),
            number: Color::Rgb(131, 165, 152),
            boolean: Color::Rgb(250, 189, 47),
            null: Color::Rgb(254, 128, 25),
            punctuation: Color::Rgb(235, 219, 178),
        },
        heart: HeartColors {
            primary: Color::Rgb(251, 73, 52),
            soft: Color::Rgb(254, 128, 25),
            deep: Color::Rgb(204, 36, 29),
            error: Color::Rgb(251, 73, 52),
        },
    };

    pub const fn builtin(id: ThemeId) -> Self {
        id.theme()
    }

    /// The same palette, resolved for what the attached terminal can show.
    pub const fn with_depth(mut self, depth: ColorDepth) -> Self {
        self.depth = depth;
        self
    }

    /// A stable color for a data value, readable on this theme's background.
    ///
    /// The hue comes from a hash of the value, so the same value always gets
    /// the same color and different values usually differ. On a truecolor
    /// terminal that hue is used directly and then lifted until it clears
    /// [`MIN_IDENTITY_CONTRAST`] against the background. Without truecolor the
    /// hue is snapped onto the xterm color cube first and lifted *within* the
    /// cube, so what the terminal displays is what lvu measured the contrast of
    /// -- rather than an approximation the emulator picked afterwards.
    pub fn value_color(self, value: &str) -> Color {
        let hash = spread_hash(stable_value_hash(value));
        let hue = hash as f64 / u64::MAX as f64 * 360.0;
        let hued = hsl_to_rgb(
            hue,
            f64::from(self.identity_saturation) / 100.0,
            f64::from(self.identity_lightness) / 100.0,
        );
        match self.depth {
            ColorDepth::TrueColor => ensure_contrast(hued, self.base_bg, MIN_IDENTITY_CONTRAST),
            // A fixed saturation and lightness put every truecolor identity on
            // one ring of the HSL cylinder. That is fine with 16 million colors
            // to spread around it, but the cube crosses that ring in only about
            // a dozen places, so quantising the hue alone would collapse 256
            // values onto a dozen colors -- distinguishing far less than the
            // terminal can actually show. The hash therefore also picks a step
            // along the ring's radius, using the cube's other dimension. Both
            // choices come from the same hash, so the value still decides its
            // own color.
            ColorDepth::Indexed256 => {
                let (saturation, lightness) = identity_variant(
                    hash,
                    f64::from(self.identity_saturation) / 100.0,
                    f64::from(self.identity_lightness) / 100.0,
                );
                cube_with_contrast(
                    hsl_to_rgb(hue, saturation, lightness),
                    self.base_bg,
                    MIN_IDENTITY_CONTRAST,
                )
            }
        }
    }

    pub fn severity_color(self, level: &str) -> Option<Color> {
        match level {
            "FATAL" => Some(self.severity.fatal),
            "ERROR" => Some(self.severity.error),
            "WARN" => Some(self.severity.warn),
            "INFO" => Some(self.severity.info),
            "DEBUG" => Some(self.severity.debug),
            "TRACE" => Some(self.severity.trace),
            _ => None,
        }
    }
}

pub fn stable_value_slot(value: &str, slots: usize) -> usize {
    if slots == 0 {
        return 0;
    }
    stable_value_hash(value) as usize % slots
}

fn stable_value_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn spread_hash(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51afd7ed558ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
    hash ^ (hash >> 33)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> Color {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let secondary = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector as u8 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let match_value = lightness - chroma / 2.0;
    let channel = |value: f64| ((value + match_value) * 255.0).round() as u8;
    Color::Rgb(channel(red), channel(green), channel(blue))
}

/// WCAG 2.x contrast ratio between two colors, or `None` when either does not
/// resolve to concrete channels (the terminal default background is unknowable).
///
/// Color-cube indexes resolve, because their displayed channels are fixed by
/// the xterm palette; the 16 system colors and the default do not, because the
/// user's terminal decides what those are.
pub fn contrast(color: Color, background: Color) -> Option<f64> {
    let (Some((red, green, blue)), Some((bg_red, bg_green, bg_blue))) =
        (resolved_rgb(color), resolved_rgb(background))
    else {
        return None;
    };
    Some(contrast_ratio(
        relative_luminance(red, green, blue),
        relative_luminance(bg_red, bg_green, bg_blue),
    ))
}

pub(crate) fn ensure_contrast(color: Color, background: Color, minimum: f64) -> Color {
    let (Color::Rgb(mut red, mut green, mut blue), Color::Rgb(bg_red, bg_green, bg_blue)) =
        (color, background)
    else {
        // The terminal-default background is unknown. Keep deterministic RGB;
        // crossterm emits truecolor ANSI directly; no indexed-color fallback is added.
        return color;
    };
    let background_luminance = relative_luminance(bg_red, bg_green, bg_blue);
    let target = if background_luminance < 0.5 { 255 } else { 0 };
    for _ in 0..24 {
        if contrast_ratio(relative_luminance(red, green, blue), background_luminance) >= minimum {
            break;
        }
        red = blend_toward(red, target);
        green = blend_toward(green, target);
        blue = blend_toward(blue, target);
    }
    Color::Rgb(red, green, blue)
}

fn blend_toward(channel: u8, target: u8) -> u8 {
    ((u16::from(channel) * 7 + u16::from(target)) / 8) as u8
}

/// The nearest color-cube color to `color` that clears `minimum` against
/// `background`, as a `Color::Indexed`.
///
/// The cube is walked, not the 24-bit space: every step lands on a color the
/// terminal can actually show, so the contrast that is measured is the contrast
/// the user sees. Steps move every channel one level toward the readable corner
/// -- white on a dark background, black on a light one -- which preserves the
/// hue's ordering between channels for as long as it can. Reaching the corner
/// ends the walk: on a background too close to both corners no cube color can
/// clear the floor, and the most readable available one is still the answer.
pub(crate) fn cube_with_contrast(color: Color, background: Color, minimum: f64) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let mut levels = [
        nearest_cube_level(red),
        nearest_cube_level(green),
        nearest_cube_level(blue),
    ];
    let Color::Rgb(bg_red, bg_green, bg_blue) = background else {
        // The terminal-default background is unknowable, so there is nothing to
        // measure against. Quantising is still right: it is what gets displayed.
        return cube_index(levels);
    };
    let background_luminance = relative_luminance(bg_red, bg_green, bg_blue);
    let target = if background_luminance < 0.5 { 5 } else { 0 };
    for _ in 0..5 {
        let [red, green, blue] = cube_rgb(levels);
        if contrast_ratio(relative_luminance(red, green, blue), background_luminance) >= minimum {
            break;
        }
        if levels.iter().all(|level| *level == target) {
            break;
        }
        for level in &mut levels {
            *level = if *level < target {
                *level + 1
            } else {
                level.saturating_sub(1)
            };
        }
    }
    cube_index(levels)
}

/// Saturation and lightness for one identity, stepped off the theme's ring.
///
/// The steps are small enough that the color still reads as the hue the hash
/// chose, and the lightness band stays inside the range the theme picked as
/// readable, so the contrast walk afterwards rarely has to move at all.
fn identity_variant(hash: u64, saturation: f64, lightness: f64) -> (f64, f64) {
    const LIGHTNESS_STEPS: [f64; 3] = [-0.13, 0.0, 0.13];
    const SATURATION_STEPS: [f64; 2] = [-0.22, 0.0];
    // Bits the hue did not use: the hue takes the value's magnitude, so the low
    // bits are still uniform and independent of it.
    let lightness_step = LIGHTNESS_STEPS[(hash % LIGHTNESS_STEPS.len() as u64) as usize];
    let saturation_step = SATURATION_STEPS[((hash / 3) % SATURATION_STEPS.len() as u64) as usize];
    (
        (saturation + saturation_step).clamp(0.0, 1.0),
        (lightness + lightness_step).clamp(0.05, 0.95),
    )
}

fn nearest_cube_level(channel: u8) -> u8 {
    let mut nearest = 0;
    for (index, level) in CUBE_LEVELS.into_iter().enumerate() {
        if channel.abs_diff(level) < channel.abs_diff(CUBE_LEVELS[usize::from(nearest)]) {
            nearest = index as u8;
        }
    }
    nearest
}

fn cube_rgb(levels: [u8; 3]) -> [u8; 3] {
    levels.map(|level| CUBE_LEVELS[usize::from(level)])
}

fn cube_index(levels: [u8; 3]) -> Color {
    Color::Indexed(CUBE_BASE + 36 * levels[0] + 6 * levels[1] + levels[2])
}

/// The concrete color an lvu color displays as, for contrast checks in tests
/// and for callers that need to reason about what the terminal will show.
pub fn resolved_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Indexed(index) if (CUBE_BASE..CUBE_BASE + 216).contains(&index) => {
            let offset = index - CUBE_BASE;
            let [red, green, blue] = cube_rgb([offset / 36, (offset / 6) % 6, offset % 6]);
            Some((red, green, blue))
        }
        _ => None,
    }
}

fn contrast_ratio(left: f64, right: f64) -> f64 {
    let (lighter, darker) = if left >= right {
        (left, right)
    } else {
        (right, left)
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn relative_luminance(red: u8, green: u8, blue: u8) -> f64 {
    [red, green, blue]
        .into_iter()
        .zip([0.2126, 0.7152, 0.0722])
        .map(|(channel, weight)| {
            let channel = f64::from(channel) / 255.0;
            let linear = if channel <= 0.04045 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            };
            linear * weight
        })
        .sum()
}
