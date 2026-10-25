fn main() {
    cc::Build::new()
        .cpp(true)
        .file("src/bindings.cpp")
        .define("NOMINMAX", None)
        .flag_if_supported("-std=c++17")
        .flag_if_supported("/std:c++17")
        .flag_if_supported("/permissive-")
        .opt_level(2)
        .warnings(false)
        .includes(["src", "vendor"])
        .compile("flip");
    println!("cargo:rerun-if-changed=src/bindings.cpp");
    println!("cargo:rerun-if-changed=src/bindings.hpp");
    println!("cargo:rerun-if-changed=vendor");
}
