//! Collects the license notices of the Rust crates linked into the app (normal dependencies
//! for the target being built) into `OUT_DIR/rust-licenses.json`, which Settings › Open source
//! licenses embeds. Like the web's `third-party-licenses.json`, it is generated, never committed.
//!
//! Output: `{"texts": [notice, …], "entries": [{name, version, license, sourceUrl, text, bundle}]}`,
//! where `text` indexes `texts` so crates sharing a notice store it once.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/fonts/Inter-LICENSE.txt");
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("rust-licenses.json");
    let manifest = match collect() {
        Ok(manifest) => manifest,
        Err(error) => {
            // An offline or unusual build still compiles; the page then lists no crates.
            println!("cargo:warning=open source licenses: {error}");
            json!({ "texts": [], "entries": [] })
        }
    };
    std::fs::write(out, manifest.to_string()).unwrap();
}

fn dir_of_manifest() -> String {
    std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default()
}

fn collect() -> Result<Value, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let target = std::env::var("TARGET").map_err(|e| e.to_string())?;
    let dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?;
    let output = Command::new(cargo)
        .current_dir(dir)
        .args(["metadata", "--format-version", "1", "--locked", "--offline", "--filter-platform", &target])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let packages: HashMap<&str, &Value> =
        metadata["packages"].as_array().into_iter().flatten().filter_map(|p| Some((p["id"].as_str()?, p))).collect();
    let nodes: HashMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| Some((n["id"].as_str()?, n)))
        .collect();
    let root = metadata["resolve"]["root"].as_str().ok_or("no root package")?;

    // Normal dependencies only: build scripts and dev-dependencies do not ship in the binary.
    let mut linked = HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        for dep in nodes.get(id).and_then(|n| n["deps"].as_array()).into_iter().flatten() {
            let normal = dep["dep_kinds"].as_array().into_iter().flatten().any(|kind| kind["kind"].is_null());
            if let Some(pkg) = dep["pkg"].as_str()
                && normal
                && linked.insert(pkg)
            {
                stack.push(pkg);
            }
        }
    }

    let mut texts: Vec<String> = Vec::new();
    let mut text_index: HashMap<String, usize> = HashMap::new();
    let mut entries: Vec<Value> = linked
        .iter()
        .filter_map(|id| packages.get(id))
        .map(|package| {
            let notice = notice_text(package);
            let index = *text_index.entry(notice.clone()).or_insert_with(|| {
                texts.push(notice);
                texts.len() - 1
            });
            let source =
                package["repository"].as_str().or(package["homepage"].as_str()).filter(|url| url.starts_with("http"));
            json!({
                "name": package["name"],
                "version": package["version"],
                "license": package["license"].as_str().unwrap_or("See notice"),
                "sourceUrl": source,
                "text": index,
                "bundle": "desktop",
            })
        })
        .collect();
    // Assets this app embeds itself (`assets/fonts`).
    let inter = std::fs::read_to_string(Path::new(&dir_of_manifest()).join("assets/fonts/Inter-LICENSE.txt"))
        .map_err(|e| e.to_string())?;
    texts.push(inter.trim().replace("\r\n", "\n"));
    entries.push(json!({
        "name": "Inter",
        "version": "4.1",
        "license": "OFL-1.1",
        "sourceUrl": "https://github.com/rsms/inter",
        "text": texts.len() - 1,
        "bundle": "assets",
    }));
    entries.sort_by(|a, b| {
        let key = |v: &Value| (v["name"].as_str().unwrap_or_default().to_lowercase(), v["version"].to_string());
        key(a).cmp(&key(b))
    });
    Ok(json!({ "texts": texts, "entries": entries }))
}

/// The crate's own LICENSE/COPYING/NOTICE files; when it ships none, a statement of its
/// declared license and authors.
fn notice_text(package: &Value) -> String {
    let manifest = package["manifest_path"].as_str().unwrap_or_default();
    let dir = Path::new(manifest).parent().unwrap_or(Path::new("."));
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
            ["license", "licence", "copying", "notice"].iter().any(|prefix| name.starts_with(prefix))
        })
        .collect();
    if let Some(declared) = package["license_file"].as_str() {
        files.push(dir.join(declared));
    }
    files.sort();
    files.dedup();
    let text: Vec<String> = files
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().replace("\r\n", "\n"))
        .filter(|text| !text.is_empty())
        .collect();
    if !text.is_empty() {
        return text.join("\n\n");
    }
    let authors: Vec<&str> = package["authors"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut statement = format!("Licensed under {}.", package["license"].as_str().unwrap_or("its declared license"));
    if !authors.is_empty() {
        statement.push_str(&format!("\n\nCopyright (c) {}", authors.join(", ")));
    }
    statement
}
