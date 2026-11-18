// No command is exposed to the web view: the secrets are only ever reached
// from Rust (`easyssh-mobile-keys`), through this plugin's native side.
const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .ios_path("ios")
        .build();
}
