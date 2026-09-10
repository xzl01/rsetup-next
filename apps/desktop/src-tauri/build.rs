use std::{fs, path::Path};

fn copy_tree(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create frontend asset directory");
    for entry in fs::read_dir(source).expect("read frontend assets") {
        let entry = entry.expect("frontend entry");
        let path = entry.path();
        let output = target.join(entry.file_name());
        if entry.file_type().expect("asset type").is_dir() {
            copy_tree(&path, &output);
        } else if entry.file_type().expect("asset type").is_file() {
            fs::copy(&path, &output).expect("copy frontend asset");
        }
    }
}

fn main() {
    let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let ui = manifest.join("../../../ui");
    let output = manifest.join("../dist");
    println!("cargo:rerun-if-changed={}", ui.display());
    if output.exists() {
        fs::remove_dir_all(&output).expect("clear generated desktop frontend");
    }
    fs::create_dir_all(&output).expect("create desktop frontend");
    for name in ["index.html", "app.js", "i18n.js", "styles.css"] {
        fs::copy(ui.join(name), output.join(name)).expect("copy production frontend");
    }
    for name in ["assets", "fonts"] {
        copy_tree(&ui.join(name), &output.join(name));
    }
    tauri_build::build()
}
