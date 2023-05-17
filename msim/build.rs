fn main() {
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rerun-if-changed=src/sim/syscall.c");
        println!("cargo:rustc-link-lib=static=syscall");
        cc::Build::new()
            .file("src/sim/syscall.c")
            .compile("syscall");
    }
}
