//! Paths to client binaries built by scripts/dev/with_stack.sh.

pub fn client_bin(name: &str) -> std::path::PathBuf {
    std::env::current_exe()
        .expect("integration test executable")
        .parent()
        .expect("deps directory")
        .parent()
        .expect("profile directory")
        .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
}
