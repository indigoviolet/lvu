//! Immutable semantic color palettes. Selection and storage live with the host;
//! this module has no process-global state or environment access.

use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ThemeId {
    #[default]
    Terminal,
    LoveDark,
    LoveLight,
}

impl ThemeId {
    pub const ALL: [Self; 3] = [Self::Terminal, Self::LoveDark, Self::LoveLight];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::LoveDark => "love-dark",
            Self::LoveLight => "love-light",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::LoveDark => "Love Dark",
            Self::LoveLight => "Love Light",
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
