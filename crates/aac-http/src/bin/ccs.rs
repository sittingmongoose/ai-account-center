//! AI Account Center command entry point.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    std::process::exit(aac_http::cli::run());
}
