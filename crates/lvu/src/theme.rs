//! Immutable semantic color palettes. Selection and storage live with the host;
//! this module has no process-global state or environment access.

use ratatui::style::Color;

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
pub struct Theme {
    pub id: ThemeId,
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
    pub heart: HeartColors,
}

impl Theme {
    pub const TERMINAL: Self = Self {
        id: ThemeId::Terminal,
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
        heart: HeartColors {
            primary: Color::Rgb(255, 111, 97),
            soft: Color::Rgb(238, 137, 124),
            deep: Color::Rgb(211, 82, 76),
            error: Color::Red,
        },
    };

    pub const LOVE_DARK: Self = Self {
        id: ThemeId::LoveDark,
        base_fg: Color::Rgb(244, 231, 234),
        base_bg: Color::Rgb(29, 22, 29),
        dialog_bg: Color::Rgb(38, 27, 36),
        input_bg: Color::Rgb(55, 37, 49),
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
        heart: HeartColors {
            primary: Color::Rgb(255, 111, 97),
            soft: Color::Rgb(238, 151, 143),
            deep: Color::Rgb(214, 77, 82),
            error: Color::Rgb(255, 94, 105),
        },
    };

    pub const LOVE_LIGHT: Self = Self {
        id: ThemeId::LoveLight,
        base_fg: Color::Rgb(58, 43, 48),
        base_bg: Color::Rgb(255, 248, 246),
        dialog_bg: Color::Rgb(250, 237, 234),
        input_bg: Color::Rgb(239, 218, 216),
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

    pub fn value_color(self, value: &str) -> Color {
        self.categorical[stable_value_slot(value, self.categorical.len())]
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
    let hash = value.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    hash as usize % slots
}
