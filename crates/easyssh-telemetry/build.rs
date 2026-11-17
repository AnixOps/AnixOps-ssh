// The DSN and the usage URL are read with `option_env!`; rebuild when they change.
fn main() {
    println!("cargo:rerun-if-env-changed=EASYSSH_SENTRY_DSN");
    println!("cargo:rerun-if-env-changed=EASYSSH_USAGE_URL");
}
