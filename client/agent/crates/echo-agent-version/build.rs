fn main() {
    println!("cargo:rerun-if-env-changed=ECHO_AGENT_VERSION");
}
