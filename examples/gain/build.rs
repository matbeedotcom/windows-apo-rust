fn main() {
    windows_apo::sdk::build_apo(&[], &[]);

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let def = std::path::Path::new(&manifest).join("gain_apo.def");
    println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
}
