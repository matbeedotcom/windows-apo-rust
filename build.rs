fn main() {
    // Emit the path to our C++ skeleton files so consumers can compile them
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let cpp_dir = format!("{}/cpp", manifest_dir.replace('\\', "/"));
    println!("cargo:cpp_dir={}", cpp_dir);
}
