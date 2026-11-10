//! The UI is embedded from `ui/dist`. A fresh checkout has no build of it
//! yet; without the directory the crate would not compile at all, so a
//! placeholder page is put there and the build says how to get the real one.

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    if !dist.join("index.html").exists() {
        std::fs::create_dir_all(&dist).expect("cannot create ui/dist");
        std::fs::write(
            dist.join("index.html"),
            "<!doctype html><meta charset=\"utf-8\"><title>Aperture</title>\
             <p>The UI has not been built. Run <code>npm --prefix ui install &amp;&amp; \
             npm --prefix ui run build</code>, then build Aperture again.</p>\n",
        )
        .expect("cannot write ui/dist/index.html");
        println!("cargo:warning=ui/dist was missing; embedded a placeholder page. Build the UI with `npm --prefix ui install && npm --prefix ui run build`.");
    }
}
