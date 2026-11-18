//! Build-time code generators (output checked in) and live tests; run
//! `cargo xtask <task>`.

mod keysyms;
mod live;
mod unicode;
mod wayland;
mod xml;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("gen-wayland") => gen_wayland(),
        Some("gen-keysyms") => gen_keysyms(),
        Some("gen-unicode") => gen_unicode(),
        Some("gen-shaders") => gen_shaders(),
        Some("live") => live::run(&std::env::args().skip(2).collect::<Vec<_>>()),
        _ => {
            eprintln!(
                "usage: cargo xtask <task>\n\ntasks:\n  gen-wayland   protocols/*.xml -> platform-linux/src/wayland/protocols/\n  gen-keysyms   data/xkbcommon-keysyms.h -> platform-linux/src/xkb/keysyms.rs\n  gen-unicode   data/unicode/*.txt -> core/src/unicode/tables.rs\n  gen-shaders   platform-linux/shaders/* -> platform-linux/src/vulkan/shaders/*.spv (glslc)\n  live [--keep] [scenario...]   real windows on this Hyprland desktop, checked by screenshot"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn gen_shaders() -> Result<(), String> {
    let root = workspace_root();
    let src = root.join("platform-linux/shaders");
    let out_dir = root.join("platform-linux/src/vulkan/shaders");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut entries: Vec<_> = std::fs::read_dir(&src).map_err(|e| format!("{}: {e}", src.display()))?.collect();
    entries.sort_by_key(|e| e.as_ref().map(|e| e.path()).ok());
    let mut n = 0;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !(name.ends_with(".vert") || name.ends_with(".frag")) {
            continue;
        }
        let out = out_dir.join(format!("{name}.spv"));
        let status = Command::new("glslc")
            .args(["--target-env=vulkan1.3", "-O", "-Werror"])
            .arg(&path)
            .arg("-o")
            .arg(&out)
            .status()
            .map_err(|e| format!("glslc: {e}"))?;
        if !status.success() {
            return Err(format!("glslc failed on {}", path.display()));
        }
        n += 1;
    }
    println!("compiled {n} shaders -> {}", out_dir.display());
    Ok(())
}

fn gen_wayland() -> Result<(), String> {
    let root = workspace_root();
    let protocols = wayland::PROTOCOLS
        .iter()
        .map(|f| wayland::load(&root.join("protocols").join(f)))
        .collect::<Result<Vec<_>, _>>()?;

    let out_dir = root.join("platform-linux/src/wayland/protocols");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let mut written = Vec::new();
    for (name, contents) in wayland::generate(&protocols).files {
        let path = out_dir.join(name);
        std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }

    rustfmt(&written)?;
    let n: usize = protocols.iter().map(|p| p.interfaces.len()).sum();
    println!("generated {} protocols, {n} interfaces -> {}", protocols.len(), out_dir.display());
    Ok(())
}

fn gen_keysyms() -> Result<(), String> {
    let root = workspace_root();
    let src = std::fs::read_to_string(root.join("data/xkbcommon-keysyms.h")).map_err(|e| e.to_string())?;
    let keysyms = keysyms::parse(&src)?;
    let path = root.join("platform-linux/src/xkb/keysyms.rs");
    std::fs::write(&path, keysyms::generate(&keysyms)).map_err(|e| format!("{}: {e}", path.display()))?;
    rustfmt(std::slice::from_ref(&path))?;
    println!("generated {} keysyms -> {}", keysyms.len(), path.display());
    Ok(())
}

fn gen_unicode() -> Result<(), String> {
    let root = workspace_root();
    let tables = unicode::build(&root.join("data/unicode"))?;
    let dir = root.join("core/src/unicode");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("tables.rs");
    std::fs::write(&path, unicode::generate(&tables)).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("generated {} -> {}", unicode::summary(&tables), path.display());
    Ok(())
}

fn rustfmt(files: &[PathBuf]) -> Result<(), String> {
    let status = Command::new("rustfmt")
        .arg("--edition")
        .arg("2024")
        .args(files)
        .status()
        .map_err(|e| format!("running rustfmt: {e}"))?;
    if status.success() { Ok(()) } else { Err("rustfmt failed".to_owned()) }
}
