use crate::ui::i18n::tr;
use crate::ui::logo::{codex_logo_lines, duck_logo_lines};
use crate::ui::output::Ui;
use crate::ui::style::Theme;
use crate::ui::text::{display_width, gradient_text};

const APP_URL: &str = "https://github.com/duckcoding-dev/duckcoding";
const SERVICE_URL: &str = "https://duckcoding.com";
const BOX_WIDTH: usize = 46;

pub fn print_banner(ui: &Ui, title: &str) {
    if ui.is_json() {
        return;
    }

    let style = ui.theme().style();
    let bar = format!("{}│{}", style.gold, style.reset);

    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push(format!(
        "  {}{}{}{}",
        style.bold,
        style.gold,
        tr(ui.lang(), "welcome"),
        style.reset
    ));
    lines.push(String::new());
    lines.push(format!(
        "  {}{}{}",
        style.dim,
        tr(ui.lang(), "tagline"),
        style.reset
    ));
    lines.push(String::new());
    lines.push(format!("  {}", tr(ui.lang(), "gui_install")));
    lines.push(String::new());
    lines.push(format!(
        "  {}{}:{}",
        style.dim,
        tr(ui.lang(), "app_url_label"),
        style.reset
    ));
    lines.push(format!("  {}{}{}", style.gold, APP_URL, style.reset));
    lines.push(String::new());
    lines.push(format!(
        "  {}{}:{}",
        style.dim,
        tr(ui.lang(), "service_label"),
        style.reset
    ));
    lines.push(format!("  {}{}{}", style.gold, SERVICE_URL, style.reset));

    println!();
    let logo_lines = match ui.theme() {
        Theme::Codex => codex_logo_lines(),
        _ => duck_logo_lines(),
    };
    for line in logo_lines {
        println!("{line}");
    }
    println!();

    for line in lines {
        println!("{bar}{line}");
    }
    println!("{bar}");
    println!();

    let installer_text = tr(ui.lang(), "installer");
    let title_text = format!("{title} {installer_text}");
    let title_width = display_width(&title_text);
    let left_pad = (BOX_WIDTH.saturating_sub(title_width)) / 2;
    let right_pad = BOX_WIDTH.saturating_sub(title_width + left_pad);

    print_border(ui, &style);
    match ui.theme() {
        Theme::Gemini => {
            let gradient = gradient_text(&title_text, ui.theme().gradient_colors(), style.reset);
            print!("  {}", " ".repeat(left_pad));
            print!("{gradient}");
            println!("{}", " ".repeat(right_pad));
        }
        _ => {
            print!("  {}{}", " ".repeat(left_pad), style.bold);
            println!("{}{}{}", style.primary, title_text, style.reset);
        }
    }
    print_border(ui, &style);
    println!();
}

pub fn print_complete(ui: &Ui) {
    if ui.is_json() {
        return;
    }

    let style = ui.theme().style();
    let text = tr(ui.lang(), "complete");
    let text_width = display_width(text);
    let left_pad = (BOX_WIDTH.saturating_sub(text_width + 4)) / 2;

    println!();
    print_border(ui, &style);
    print!("  {}", " ".repeat(left_pad));
    match ui.theme() {
        Theme::Gemini => {
            print!("{}[ ", style.bold);
            let gradient = gradient_text(text, ui.theme().gradient_colors(), style.reset);
            print!("{gradient}");
            println!(" ]{}", style.reset);
        }
        _ => {
            println!("{}{}[ {} ]{}", style.bold, style.green, text, style.reset);
        }
    }
    print_border(ui, &style);
    println!();
}

fn print_border(ui: &Ui, style: &crate::ui::style::ThemeStyle) {
    match ui.theme() {
        Theme::Gemini => {
            let half = BOX_WIDTH / 2 + 1;
            println!(
                "  {}{}{}{}{}",
                style.primary,
                "─".repeat(half),
                style.accent,
                "─".repeat(half),
                style.reset
            );
        }
        _ => {
            println!(
                "  {}{}{}",
                style.primary,
                "─".repeat(BOX_WIDTH),
                style.reset
            );
        }
    }
}
