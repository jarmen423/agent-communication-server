//! Embed all canonical MCP modules, including modules added by other workstreams.
use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../mcp_server");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut paths: Vec<_> = fs::read_dir(&root)
        .expect("canonical MCP runtime")
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "py"))
        .collect();
    paths.sort();
    let mut source = String::from("const SOURCES: &[(&str, &[u8])] = &[\n");
    for path in paths {
        source.push_str(&format!(
            "({:?}, include_bytes!({:?})),\n",
            path.file_name().unwrap().to_str().unwrap(),
            path.canonicalize().unwrap()
        ));
    }
    source.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("mcp_sources.rs"),
        source,
    )
    .expect("write embedded runtime manifest");
}
