use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    for asset in ["app.rc", "app.manifest", "app.ico"] {
        println!("cargo:rerun-if-changed=assets/{asset}");
    }
    if env::var("TARGET").as_deref() != Ok("x86_64-pc-windows-gnu") {
        return;
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("app.o");
    let status = Command::new("x86_64-w64-mingw32-windres")
        .current_dir("assets")
        .args(["app.rc", "-O", "coff", "-o"])
        .arg(&output)
        .status()
        .expect("run x86_64-w64-mingw32-windres");
    assert!(status.success(), "compile Windows resources");
    println!("cargo:rustc-link-arg-bin=ipv6-ddns={}", output.display());
}
