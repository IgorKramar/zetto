pub mod app;
pub mod cli;
pub mod config;
pub mod editor;
pub mod error;
pub mod format;
pub mod index;
pub mod interrupt;
pub mod note;
pub mod resolver;
pub mod tui;
pub mod vault;

pub(crate) fn safe(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && !matches!(c, '\n' | '\t') {
                '�'
            } else {
                c
            }
        })
        .collect()
}
