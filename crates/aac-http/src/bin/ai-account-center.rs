//! AI Account Center command entry point.

#[global_allocator]
static GLOBAL: aac_http::allocator::Selected = aac_http::allocator::SELECTED;

fn main() {
    aac_http::allocator::tune();
    std::process::exit(aac_http::cli::run());
}
