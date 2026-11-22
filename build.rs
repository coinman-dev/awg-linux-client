// Optionally embeds the AmneziaWG backend (see scripts/build-backend.sh) into the executable:
//   vendor/amneziawg-go, vendor/awg, vendor/awg-quick
// Set AWG_GUI_NO_EMBED=1 to build without them.
use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=AWG_GUI_NO_EMBED");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let no_embed = env::var_os("AWG_GUI_NO_EMBED").is_some();
    let mut code = String::new();
    for (konst, file) in [("AWG_GO", "amneziawg-go"), ("AWG", "awg"), ("AWG_QUICK", "awg-quick")] {
        let p = manifest.join("vendor").join(file);
        println!("cargo:rerun-if-changed={}", p.display());
        if p.is_file() && !no_embed {
            code += &format!("pub const {konst}: Option<&[u8]> = Some(include_bytes!({:?}));\n", p);
        } else {
            code += &format!("pub const {konst}: Option<&[u8]> = None;\n");
        }
    }
    fs::write(PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded.rs"), code).unwrap();
}
