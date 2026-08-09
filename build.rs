fn main() {
    built::write_built_file().expect("Failed to acquire build-time information");

    // Windows sizes the main thread's stack from the PE header's
    // SizeOfStackReserve, which defaults to 1 MiB — it does NOT get the 8 MiB
    // main thread the `[profile.test]` note in Cargo.toml assumes (that's a
    // Unix default). At opt-level 0 the clap-derive command-tree build
    // (`Cli::command()` and the `augment_subcommands` chain) transiently uses
    // ~1.9 MiB, so on Windows every debug-built `xv` command aborts with
    // STATUS_STACK_OVERFLOW ("thread 'main' has overflowed its stack") before
    // main's first line runs. Reserve 8 MiB to match the Unix main thread.
    //
    // `rustc-link-arg-bins` scopes this to this package's binaries, so it does
    // not invalidate the dependency build cache the way a global RUSTFLAGS
    // entry in `.cargo/config.toml` would. Reserve is address space only;
    // pages are committed on demand, so this costs nothing at runtime.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bins=/STACK:8388608"),
            Ok("gnu") => println!("cargo:rustc-link-arg-bins=-Wl,--stack,8388608"),
            _ => {}
        }
    }
}
