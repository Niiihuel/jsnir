fn main() {
    println!("cargo:rerun-if-changed=context.c");
    cc::Build::new().file("context.c").compile("jsnir_gl");
    println!("cargo:rustc-link-lib=SDL2");
}
