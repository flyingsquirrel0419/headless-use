fn main() {
    println!(
        "cargo:rustc-env=HEADLESS_USE_BUILD_TARGET={}",
        std::env::var("TARGET").unwrap_or_default()
    );
    let version = option_env!("CARGO_PKG_VERSION").unwrap_or("0.0.0");
    println!("cargo:rustc-env=HEADLESS_USE_PKG_VERSION={version}");
}
