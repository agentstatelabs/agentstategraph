//! Build script for the Python binding.
//!
//! PyO3's `extension-module` feature suppresses the link to `libpython`,
//! which is what we want — the extension is loaded *by* the running
//! Python interpreter, which provides the symbols. But macOS's default
//! linker rejects undefined symbols at build time, so we have to tell
//! it explicitly to tolerate them and resolve at `dlopen()` time.
//!
//! Linux and Windows don't need this; their default linkers already
//! allow the symbols to be resolved lazily.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-undefined");
        println!("cargo:rustc-link-arg=dynamic_lookup");
    }
}
