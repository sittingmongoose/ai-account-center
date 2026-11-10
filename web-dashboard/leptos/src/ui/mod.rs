//! The design system of the redesign (DESIGN-SYSTEM.md section 5) as Leptos components, and the page shell.

pub mod charts;
pub mod chrome;
pub mod device;
mod icons;
mod parts;

pub use chrome::{ChromeInfo, FrameShell, PageShell, apply_theme, stored_theme};
pub use icons::{Icon, Logo, Mark, PlatformGlyph, ProviderMark, mark_scale};
pub use parts::*;
