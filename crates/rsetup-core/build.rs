use std::path::Path;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }

    // Check if libnvme is available for linking.
    // Try standard library paths for libnvme.a or libnvme.so.
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let search_paths = [
        format!("/usr/lib/{}-linux-gnu", match arch.as_str() {
            "x86_64" => "x86_64",
            "aarch64" => "aarch64",
            "arm" => "arm-linux-gnueabihf",
            "riscv64" => "riscv64",
            _ => "",
        }),
        "/usr/local/lib".to_string(),
        "/usr/lib".to_string(),
    ];

    let mut found_static = false;
    let mut found_shared = false;

    for dir in &search_paths {
        if dir.is_empty() {
            continue;
        }
        let static_lib = Path::new(dir).join("libnvme.a");
        if static_lib.exists() {
            println!("cargo:rustc-link-search=native={}", dir);
            println!("cargo:rustc-link-lib=static=nvme");
            found_static = true;
            break;
        }
        let shared_lib = Path::new(dir).join("libnvme.so");
        if shared_lib.exists() {
            println!("cargo:rustc-link-search=native={}", dir);
            println!("cargo:rustc-link-lib=nvme");
            found_shared = true;
            break;
        }
    }

    if found_static || found_shared {
        println!("cargo:rustc-cfg=feature=\"libnvme\"");
    }
}
