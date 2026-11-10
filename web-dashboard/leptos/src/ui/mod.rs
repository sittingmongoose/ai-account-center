//! The design system of the redesign (DESIGN-SYSTEM.md section 5) as Leptos components, and the page shell.

pub mod charts;
pub mod chrome;
mod controls;
pub mod device;
mod icons;
pub mod meter;
mod parts;

pub use chrome::{ChromeInfo, FrameShell, PageShell, apply_theme, stored_theme};
pub use controls::*;
pub use icons::{Icon, IconOf, Logo, Mark, PlatformGlyph, ProviderMark};
pub use meter::{MeterMode, MeterView, UsageMeter, motion_on};
pub use parts::*;
