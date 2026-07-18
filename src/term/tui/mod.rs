//! Pure fullscreen Ratatui state and rendering.

mod app;
mod composer;
mod transcript;
mod view;

pub use app::{InputAction, TuiApp};

#[cfg(test)]
mod composer_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod tests;
