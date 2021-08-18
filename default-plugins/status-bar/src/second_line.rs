use ansi_term::{
    ANSIStrings,
    Color::{Fixed, RGB},
    Style,
};
use zellij_tile::prelude::*;

use crate::{LinePart, MORE_MSG};

fn full_length_shortcut(
    is_first_shortcut: bool,
    letter: &str,
    description: &str,
    palette: Option<Palette>,
) -> LinePart {
    let white_color_style = match palette {
        Some(Palette {
            white: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            white: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let green_color_style = match palette {
        Some(Palette {
            green: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            green: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let separator = if is_first_shortcut { " " } else { " / " };
    let separator = white_color_style.paint(separator);
    let shortcut_len = letter.chars().count() + 3; // 2 for <>'s around shortcut, 1 for the space
    let shortcut_left_separator = white_color_style.paint("<");
    let shortcut = green_color_style.bold().paint(letter);
    let shortcut_right_separator = white_color_style.paint("> ");
    let description_len = description.chars().count();
    let description = white_color_style.bold().paint(description);
    let len = shortcut_len + description_len + separator.chars().count();
    LinePart {
        part: format!(
            "{}",
            ANSIStrings(&[
                separator,
                shortcut_left_separator,
                shortcut,
                shortcut_right_separator,
                description
            ])
        ),
        len,
    }
}

fn first_word_shortcut(
    is_first_shortcut: bool,
    letter: &str,
    description: &str,
    palette: Option<Palette>,
) -> LinePart {
    let white_color_style = match palette {
        Some(Palette {
            white: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            white: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let green_color_style = match palette {
        Some(Palette {
            green: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            green: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let separator = if is_first_shortcut { " " } else { " / " };
    let separator = white_color_style.paint(separator);
    let shortcut_len = letter.chars().count() + 3; // 2 for <>'s around shortcut, 1 for the space
    let shortcut_left_separator = white_color_style.paint("<");
    let shortcut = green_color_style.bold().paint(letter);
    let shortcut_right_separator = white_color_style.paint("> ");
    let description_first_word = description.split(' ').next().unwrap_or("");
    let description_first_word_length = description_first_word.chars().count();
    let description_first_word = white_color_style.bold().paint(description_first_word);
    let len = shortcut_len + description_first_word_length + separator.chars().count();
    LinePart {
        part: format!(
            "{}",
            ANSIStrings(&[
                separator,
                shortcut_left_separator,
                shortcut,
                shortcut_right_separator,
                description_first_word,
            ])
        ),
        len,
    }
}
fn quicknav_full(palette: Option<Palette>) -> LinePart {
    let text_first_part = " Tip: ";
    let alt = "Alt";
    let text_second_part = " + ";
    let new_pane_shortcut = "n";
    let text_third_part = " => open new pane. ";
    let second_alt = "Alt";
    let text_fourth_part = " + ";
    let brackets_navigation = "[]";
    let text_fifth_part = " or ";
    let hjkl_navigation = "hjkl";
    let text_sixths_part = " => navigate between panes.";
    let len = text_first_part.chars().count()
        + alt.chars().count()
        + text_second_part.chars().count()
        + new_pane_shortcut.chars().count()
        + text_third_part.chars().count()
        + second_alt.chars().count()
        + text_fourth_part.chars().count()
        + brackets_navigation.chars().count()
        + text_fifth_part.chars().count()
        + hjkl_navigation.chars().count()
        + text_sixths_part.chars().count();
    let green_color_style = match palette {
        Some(Palette {
            green: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            green: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let orange_color_style = match palette {
        Some(Palette {
            orange: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            orange: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    LinePart {
        part: format!(
            "{}{}{}{}{}{}{}{}{}{}{}",
            text_first_part,
            orange_color_style.bold().paint(alt),
            text_second_part,
            green_color_style.bold().paint(new_pane_shortcut),
            text_third_part,
            orange_color_style.bold().paint(second_alt),
            text_fourth_part,
            green_color_style.bold().paint(brackets_navigation),
            text_fifth_part,
            green_color_style.bold().paint(hjkl_navigation),
            text_sixths_part,
        ),
        len,
    }
}

fn quicknav_medium(palette: Option<Palette>) -> LinePart {
    let text_first_part = " Tip: ";
    let alt = "Alt";
    let text_second_part = " + ";
    let new_pane_shortcut = "n";
    let text_third_part = " => new pane. ";
    let second_alt = "Alt";
    let text_fourth_part = " + ";
    let brackets_navigation = "[]";
    let text_fifth_part = " or ";
    let hjkl_navigation = "hjkl";
    let text_sixths_part = " => navigate.";
    let len = text_first_part.chars().count()
        + alt.chars().count()
        + text_second_part.chars().count()
        + new_pane_shortcut.chars().count()
        + text_third_part.chars().count()
        + second_alt.chars().count()
        + text_fourth_part.chars().count()
        + brackets_navigation.chars().count()
        + text_fifth_part.chars().count()
        + hjkl_navigation.chars().count()
        + text_sixths_part.chars().count();
    let green_color_style = match palette {
        Some(Palette {
            green: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            green: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let orange_color_style = match palette {
        Some(Palette {
            orange: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            orange: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    LinePart {
        part: format!(
            "{}{}{}{}{}{}{}{}{}{}{}",
            text_first_part,
            orange_color_style.bold().paint(alt),
            text_second_part,
            green_color_style.bold().paint(new_pane_shortcut),
            text_third_part,
            orange_color_style.bold().paint(second_alt),
            text_fourth_part,
            green_color_style.bold().paint(brackets_navigation),
            text_fifth_part,
            green_color_style.bold().paint(hjkl_navigation),
            text_sixths_part,
        ),
        len,
    }
}

fn quicknav_short(palette: Option<Palette>) -> LinePart {
    let text_first_part = " QuickNav: ";
    let alt = "Alt";
    let text_second_part = " + ";
    let new_pane_shortcut = "n";
    let text_third_part = "/";
    let brackets_navigation = "[]";
    let text_fifth_part = "/";
    let hjkl_navigation = "hjkl";
    let len = text_first_part.chars().count()
        + alt.chars().count()
        + text_second_part.chars().count()
        + new_pane_shortcut.chars().count()
        + text_third_part.chars().count()
        + brackets_navigation.chars().count()
        + text_fifth_part.chars().count()
        + hjkl_navigation.chars().count();
    let green_color_style = match palette {
        Some(Palette {
            green: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            green: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let orange_color_style = match palette {
        Some(Palette {
            orange: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            orange: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    LinePart {
        part: format!(
            "{}{}{}{}{}{}{}{}",
            text_first_part,
            orange_color_style.bold().paint(alt),
            text_second_part,
            green_color_style.bold().paint(new_pane_shortcut),
            text_third_part,
            green_color_style.bold().paint(brackets_navigation),
            text_fifth_part,
            green_color_style.bold().paint(hjkl_navigation),
        ),
        len,
    }
}

fn locked_interface_indication(palette: Option<Palette>) -> LinePart {
    let locked_text = " -- INTERFACE LOCKED -- ";
    let locked_text_len = locked_text.chars().count();
    let white_color_style = match palette {
        Some(Palette {
            white: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            white: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let locked_styled_text = white_color_style.bold().paint(locked_text);
    LinePart {
        part: format!("{}", locked_styled_text),
        len: locked_text_len,
    }
}

fn select_pane_shortcut(is_first_shortcut: bool, palette: Option<Palette>) -> LinePart {
    let shortcut = "ENTER";
    let description = "Select pane";
    let separator = if is_first_shortcut { " " } else { " / " };
    let white_color_style = match palette {
        Some(Palette {
            white: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            white: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let orange_color_style = match palette {
        Some(Palette {
            orange: PaletteColor::Rgb((r, g, b)),
            ..
        }) => Style::new().fg(RGB(r, g, b)),
        Some(Palette {
            orange: PaletteColor::EightBit(color),
            ..
        }) => Style::new().fg(Fixed(color)),
        _ => Style::new(),
    };
    let separator = white_color_style.paint(separator);
    let shortcut_len = shortcut.chars().count() + 3; // 2 for <>'s around shortcut, 1 for the space
    let shortcut_left_separator = white_color_style.paint("<");
    let shortcut = orange_color_style.bold().paint(shortcut);
    let shortcut_right_separator = white_color_style.paint("> ");
    let description_len = description.chars().count();
    let description = white_color_style.bold().paint(description);
    let len = shortcut_len + description_len + separator.chars().count();
    LinePart {
        part: format!(
            "{}",
            ANSIStrings(&[
                separator,
                shortcut_left_separator,
                shortcut,
                shortcut_right_separator,
                description
            ])
        ),
        len,
    }
}

fn full_shortcut_list(help: &ModeInfo) -> LinePart {
    match help.mode {
        InputMode::Normal => quicknav_full(help.palette),
        InputMode::Locked => locked_interface_indication(help.palette),
        _ => {
            let mut line_part = LinePart::default();
            for (i, (letter, description)) in help.keybinds.iter().enumerate() {
                let shortcut = full_length_shortcut(i == 0, letter, description, help.palette);
                line_part.len += shortcut.len;
                line_part.part = format!("{}{}", line_part.part, shortcut,);
            }
            let select_pane_shortcut = select_pane_shortcut(help.keybinds.is_empty(), help.palette);
            line_part.len += select_pane_shortcut.len;
            line_part.part = format!("{}{}", line_part.part, select_pane_shortcut,);
            line_part
        }
    }
}

fn shortened_shortcut_list(help: &ModeInfo) -> LinePart {
    match help.mode {
        InputMode::Normal => quicknav_medium(help.palette),
        InputMode::Locked => locked_interface_indication(help.palette),
        _ => {
            let mut line_part = LinePart::default();
            for (i, (letter, description)) in help.keybinds.iter().enumerate() {
                let shortcut = first_word_shortcut(i == 0, letter, description, help.palette);
                line_part.len += shortcut.len;
                line_part.part = format!("{}{}", line_part.part, shortcut,);
            }
            let select_pane_shortcut = select_pane_shortcut(help.keybinds.is_empty(), help.palette);
            line_part.len += select_pane_shortcut.len;
            line_part.part = format!("{}{}", line_part.part, select_pane_shortcut,);
            line_part
        }
    }
}

fn best_effort_shortcut_list(help: &ModeInfo, max_len: usize) -> LinePart {
    match help.mode {
        InputMode::Normal => {
            let line_part = quicknav_short(help.palette);
            if line_part.len <= max_len {
                line_part
            } else {
                LinePart::default()
            }
        }
        InputMode::Locked => {
            let line_part = locked_interface_indication(help.palette);
            if line_part.len <= max_len {
                line_part
            } else {
                LinePart::default()
            }
        }
        _ => {
            let mut line_part = LinePart::default();
            for (i, (letter, description)) in help.keybinds.iter().enumerate() {
                let shortcut = first_word_shortcut(i == 0, letter, description, help.palette);
                if line_part.len + shortcut.len + MORE_MSG.chars().count() > max_len {
                    // TODO: better
                    line_part.part = format!("{}{}", line_part.part, MORE_MSG);
                    line_part.len += MORE_MSG.chars().count();
                    break;
                }
                line_part.len += shortcut.len;
                line_part.part = format!("{}{}", line_part.part, shortcut);
            }
            let select_pane_shortcut = select_pane_shortcut(help.keybinds.is_empty(), help.palette);
            if line_part.len + select_pane_shortcut.len <= max_len {
                line_part.len += select_pane_shortcut.len;
                line_part.part = format!("{}{}", line_part.part, select_pane_shortcut,);
            }
            line_part
        }
    }
}

pub fn keybinds(help: &ModeInfo, max_width: usize) -> LinePart {
    let full_shortcut_list = full_shortcut_list(help);
    if full_shortcut_list.len <= max_width {
        return full_shortcut_list;
    }
    let shortened_shortcut_list = shortened_shortcut_list(help);
    if shortened_shortcut_list.len <= max_width {
        return shortened_shortcut_list;
    }
    best_effort_shortcut_list(help, max_width)
}
