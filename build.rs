// Optionally embeds the AmneziaWG backend (see packaging/build-backend.sh) into the executable:
//   vendor/amneziawg-go, vendor/awg, vendor/awg-quick (+ vendor/versions.txt)
// Set AWG_GUI_NO_EMBED=1 to build without them.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=AWG_GUI_NO_EMBED");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let no_embed = env::var_os("AWG_GUI_NO_EMBED").is_some();
    let mut code = String::new();
    let mut embedded = false;
    for (konst, file) in [("AWG_GO", "amneziawg-go"), ("AWG", "awg"), ("AWG_QUICK", "awg-quick")] {
        let p = manifest.join("vendor").join(file);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.is_file() && !no_embed {
            code += &format!("pub const {konst}: Option<&[u8]> = Some(include_bytes!({:?}));\n", p);
            embedded = true;
        } else {
            code += &format!("pub const {konst}: Option<&[u8]> = None;\n");
        }
    }
    let versions = manifest.join("vendor/versions.txt");
    println!("cargo:rerun-if-changed={}", versions.display());
    match fs::read_to_string(&versions) {
        Ok(v) if embedded => code += &format!("pub const BACKEND_VERSIONS: Option<&str> = Some({:?});\n", v.trim()),
        _ => code += "pub const BACKEND_VERSIONS: Option<&str> = None;\n",
    }
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded.rs"), code).unwrap();
}
