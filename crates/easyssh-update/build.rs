// The trusted public keys are read with `option_env!`; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-env-changed=EASYSSH_UPDATE_PUBLIC_KEYS");
}
