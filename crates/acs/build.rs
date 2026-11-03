//! Embed the canonical Python runtimes so a release `acs` needs no checkout:
//! every MCP module, and every worker entrypoint plus all repo modules it
//! imports (found by scanning import lines, so new imports are picked up).
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
};

/// Workers whose third-party SDK is not in the private venv `acs` creates.
const CHECKOUT_ONLY: &[&str] = &["cursor_worker.py"];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.join("../..").canonicalize().unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    mcp(&repo, &out);
    worker(&repo, &out);
}

fn manifest(name: &str, files: &[(String, PathBuf)]) -> String {
    let mut source = format!("const {name}: &[(&str, &[u8])] = &[\n");
    for (rel, path) in files {
        source.push_str(&format!("({rel:?}, include_bytes!({path:?})),\n"));
    }
    source + "];\n"
}

fn mcp(repo: &Path, out: &Path) {
    let root = repo.join("mcp_server");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files: Vec<_> = fs::read_dir(&root)
        .expect("canonical MCP runtime")
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "py"))
        .map(|p| (p.file_name().unwrap().to_str().unwrap().to_owned(), p))
        .collect();
    files.sort();
    fs::write(out.join("mcp_sources.rs"), manifest("SOURCES", &files))
        .expect("write embedded runtime manifest");
}

/// Top-level module names named by `import a, b` / `from a.b import c` lines.
fn imports(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines().map(str::trim_start) {
        let modules: Vec<&str> = if let Some(rest) = line.strip_prefix("from ") {
            vec![rest]
        } else if let Some(rest) = line.strip_prefix("import ") {
            rest.split(',').collect()
        } else {
            continue;
        };
        for module in modules {
            let name: String = module
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

fn worker(repo: &Path, out: &Path) {
    let mut queue: Vec<String> = fs::read_dir(repo)
        .expect("repo root")
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with("_worker.py") && !CHECKOUT_ONLY.contains(&n.as_str()))
        .collect();
    let mut seen = BTreeSet::new();
    while let Some(rel) = queue.pop() {
        if !seen.insert(rel.clone()) {
            continue;
        }
        let text = fs::read_to_string(repo.join(&rel)).expect("worker module");
        for name in imports(&text) {
            if repo.join(format!("{name}.py")).is_file() {
                queue.push(format!("{name}.py"));
            } else if repo.join(&name).join("__init__.py").is_file() {
                // A repo package is embedded whole: lazy and relative imports
                // inside it then resolve without further scanning.
                for entry in fs::read_dir(repo.join(&name)).unwrap() {
                    let file = entry.unwrap().file_name().into_string().unwrap();
                    if file.ends_with(".py") {
                        queue.push(format!("{name}/{file}"));
                    }
                }
            }
        }
    }
    // FNV-1a over names and contents: the private runtime directory changes
    // whenever the embedded code does, even without a version bump.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut files = Vec::new();
    for rel in seen {
        let path = repo.join(&rel).canonicalize().unwrap();
        println!("cargo:rerun-if-changed={}", path.display());
        for byte in rel.bytes().chain([0]).chain(fs::read(&path).unwrap()) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
        files.push((rel, path));
    }
    let tag = format!(
        "{}-{:012x}",
        env::var("CARGO_PKG_VERSION").unwrap(),
        hash & 0xffff_ffff_ffff
    );
    let source =
        manifest("WORKER_SOURCES", &files) + &format!("const WORKER_TAG: &str = {tag:?};\n");
    fs::write(out.join("worker_sources.rs"), source).expect("write embedded worker manifest");
}
