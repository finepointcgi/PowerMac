//! PowerRename (bulk rename) and New+ (new files from templates), ported
//! from PowerToys, plus their Finder / Explorer integration.

pub mod config;
pub mod integration;
pub mod newplus;
pub mod rename;
pub mod vars;

pub use config::{Config, NewPlusConfig, RenameConfig};
pub use integration::{
    finder_integration_installed, install_finder_integration, uninstall_finder_integration,
};
pub use newplus::{new_from_template, templates_dir};

pub mod rename_ui;
mod ui_common;

pub use rename_ui::run_rename_window;

pub mod newplus_ui;
#[cfg(test)]
mod shots;

pub use newplus_ui::run_new_window;
