use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    stage_shim();
    tauri_build::build()
}

/// Tauri looks for `binaries/telegram-mcp-<triple>` and copies it next to the app.
/// The shim is built in a separate target dir so this build does not lock the outer one.
fn stage_shim() {
    let triple = env::var("TARGET").expect("TARGET");
    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_owned());
    let extension = if triple.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let dest_dir = PathBuf::from("binaries");
    let dest = dest_dir.join(format!("telegram-mcp-{triple}{extension}"));
    fs::create_dir_all(&dest_dir).expect("не удалось создать каталог binaries");

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .expect("не найден каталог workspace");
    let sidecar_target = workspace.join("target").join("sidecar");
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut command = Command::new(cargo);
    command
        .current_dir(&workspace)
        .env("CARGO_TARGET_DIR", &sidecar_target)
        .args([
            "build",
            "-p",
            "telegram-mcp",
            "--target",
            &triple,
            "--manifest-path",
            "Cargo.toml",
        ]);
    if profile == "release" {
        command.arg("--release");
    }
    let status = command
        .status()
        .expect("не удалось запустить сборку telegram-mcp");
    if !status.success() {
        panic!("не удалось собрать telegram-mcp");
    }

    let folder = if profile == "release" {
        "release"
    } else {
        "debug"
    };
    let artifact = sidecar_target
        .join(&triple)
        .join(folder)
        .join(format!("telegram-mcp{extension}"));
    fs::copy(&artifact, &dest)
        .unwrap_or_else(|_| panic!("не удалось скопировать {}", artifact.display()));
    println!("cargo:rerun-if-changed=../crates/stdio");
    println!("cargo:rerun-if-changed=../crates/ipc");
}
