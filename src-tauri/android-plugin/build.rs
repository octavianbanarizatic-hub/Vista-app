// Nothing is called from JavaScript; the Rust side talks to the Kotlin plugin
// directly, so there are no commands or permissions to generate.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).android_path("android").build();
}
