fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/sim/sys-getrandom.c");
        println!("cargo:rustc-link-lib=static=sys-getrandom");
        cc::Build::new()
            .file("src/sim/sys-getrandom.c")
            .compile("sys-getrandom");
    }
}
