#[derive(Clone, Copy, Debug)]
pub enum Theme {
    Codex,
    Claude,
    Gemini,
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeStyle {
    pub reset: &'static str,
    pub bold: &'static str,
    pub dim: &'static str,
    pub red: &'static str,
    pub green: &'static str,
    pub white: &'static str,
    pub primary: &'static str,
    pub accent: &'static str,
    pub gold: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct ThemeSymbols {
    pub info: &'static str,
    pub success: &'static str,
    pub update: &'static str,
}

#[derive(Clone, Copy, Debug)]
pub struct ProgressSymbols {
    pub spinner: &'static [&'static str],
    pub success: &'static str,
    pub error: &'static str,
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[0;31m";
const GREEN: &str = "\x1b[0;32m";
const WHITE: &str = "\x1b[1;37m";
const GOLD: &str = "\x1b[38;2;253;216;31m";

impl Theme {
    pub fn style(self) -> ThemeStyle {
        match self {
            Theme::Codex => ThemeStyle {
                reset: RESET,
                bold: BOLD,
                dim: DIM,
                red: RED,
                green: GREEN,
                white: WHITE,
                primary: "\x1b[38;5;79m",
                accent: "\x1b[38;5;115m",
                gold: GOLD,
            },
            Theme::Claude => ThemeStyle {
                reset: RESET,
                bold: BOLD,
                dim: DIM,
                red: RED,
                green: GREEN,
                white: WHITE,
                primary: "\x1b[38;5;167m",
                accent: "\x1b[38;5;173m",
                gold: GOLD,
            },
            Theme::Gemini => ThemeStyle {
                reset: RESET,
                bold: BOLD,
                dim: DIM,
                red: RED,
                green: GREEN,
                white: WHITE,
                primary: "\x1b[38;5;74m",
                accent: "\x1b[38;5;168m",
                gold: GOLD,
            },
        }
    }

    pub fn symbols(self) -> ThemeSymbols {
        match self {
            Theme::Codex => ThemeSymbols {
                info: ">",
                success: "•",
                update: "^",
            },
            Theme::Claude => ThemeSymbols {
                info: ">",
                success: "*",
                update: "^",
            },
            Theme::Gemini => ThemeSymbols {
                info: ">",
                success: "●",
                update: "^",
            },
        }
    }

    pub fn progress_symbols(self) -> ProgressSymbols {
        match self {
            Theme::Codex => ProgressSymbols {
                spinner: &["·", "•", "·"],
                success: "•",
                error: "•",
            },
            Theme::Claude => ProgressSymbols {
                spinner: &["·", "✢", "✶", "✳", "✻", "✽"],
                success: "*",
                error: "x",
            },
            Theme::Gemini => ProgressSymbols {
                spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
                success: "●",
                error: "○",
            },
        }
    }

    pub fn shimmer_colors(self) -> &'static [u8] {
        match self {
            Theme::Codex => &[240, 244, 248, 255, 255, 248, 244, 240],
            _ => &[],
        }
    }

    pub fn gradient_colors(self) -> &'static [u8] {
        match self {
            Theme::Gemini => &[74, 74, 104, 104, 132, 168, 168],
            _ => &[],
        }
    }

    pub fn progress_circle(self, pct: u8) -> &'static str {
        if !matches!(self, Theme::Claude) {
            return "";
        }
        match pct {
            0..=11 => "○",
            12..=36 => "◔",
            37..=61 => "◑",
            62..=86 => "◕",
            _ => "●",
        }
    }

    pub fn spinner_colors(self) -> &'static [u8] {
        match self {
            Theme::Gemini => &[74, 104, 168],
            _ => &[],
        }
    }
}
