//! The design system of the redesign (DESIGN-SYSTEM.md section 5) as Leptos components, and the page shell.

mod chrome;
pub mod device;
mod icons;
mod parts;

pub use chrome::{ChromeInfo, Shell};
pub use icons::{Icon, Mark, PlatformGlyph};
pub use parts::*;
