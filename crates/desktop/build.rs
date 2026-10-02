// Embeds the application icon into tapbridge.exe. Skipped silently when the
// generated .ico is missing (run tools/gen-icons first); the exe still builds.
fn main() {
    if !cfg!(windows) {
        return;
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let icon = std::path::Path::new(&manifest).join("assets/tapbridge.ico");
    if !icon.exists() {
        println!("cargo:warning=tapbridge.ico missing; run tools/gen-icons to generate assets");
        return;
    }
    winresource::WindowsResource::new()
        .set_icon(icon.to_str().unwrap())
        .compile()
        .expect("embed tapbridge.ico into tapbridge.exe");
}
