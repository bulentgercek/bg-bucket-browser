// Build script: runs before the crate is compiled.
fn main() {
    // A `bg-core/` folder beside `src-tauri/` marks a development checkout,
    // compiled in as the `bg_dev` cfg; the dev log then defaults to verbose.
    println!("cargo::rustc-check-cfg=cfg(bg_dev)");
    if std::path::Path::new("../bg-core").is_dir() {
        println!("cargo::rustc-cfg=bg_dev");
    }

    // Generates the glue code for tauri.conf.json, capabilities and icons.
    tauri_build::build();
}
