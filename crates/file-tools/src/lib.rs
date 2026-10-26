//! PowerRename (bulk rename), New+ (new files from templates) and Image
//! Resizer, ported from PowerToys, the "Finder is missing basics" helpers
//! (Open in Terminal / editor, Copy path, hidden files, Eject all, what is
//! using a file), plus their Finder / Explorer integration.

pub mod config;
pub mod finder;
pub mod integration;
pub mod newplus;
pub mod rename;
pub mod resize;
pub mod vars;

pub use config::{Config, FinderConfig, NewPlusConfig, RenameConfig, ResizeConfig};
pub use finder::{
    copy_paths, eject_all, external_volumes, hidden_files_shown, open_in_editor, open_in_terminal,
    toggle_hidden_files, who_is_using, CopyStyle, EjectReport, ProcessInfo, VolumeInfo,
};
pub use integration::{
    finder_integration_installed, finder_integration_needs_update, install_finder_integration,
    uninstall_finder_integration,
};
pub use newplus::{new_from_template, templates_dir};
pub use resize::{resize, ResizeOptions};

pub mod rename_ui;
mod ui_common;

pub use rename_ui::run_rename_window;

pub mod newplus_ui;
pub mod resize_ui;
#[cfg(test)]
mod shots;

pub use newplus_ui::run_new_window;
pub use resize_ui::run_resize_window;
