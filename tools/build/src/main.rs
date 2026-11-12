use anyhow::{Context, Result, ensure};
use std::{env, path::Path, process::Command};
use wasm_bindgen_cli_support::Bindgen;

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("Could not find the Freeflow workspace")?;
    env::set_current_dir(root)?;
    let target = env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into());
    let target = std::path::absolute(target)?;
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut arguments = env::args_os().skip(1).peekable();
    let build_only = arguments.next_if(|arg| arg == "--build-only").is_some();

    let status = Command::new(&cargo)
        .args([
            "build",
            "--locked",
            "-p",
            "freeflow-web",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .status()?;
    ensure!(
        status.success(),
        "Could not build the Freeflow browser interface. Install the wasm32-unknown-unknown Rust target."
    );
    let assets = target.join("freeflow-web");
    Bindgen::new()
        .input_path(target.join("wasm32-unknown-unknown/release/freeflow_web.wasm"))
        .out_name("freeflow_web")
        .web(true)?
        .typescript(false)
        .generate(&assets)?;
    if build_only {
        return Ok(());
    }
    let status = Command::new(cargo)
        .args(["run", "--locked", "-p", "freeflow", "--"])
        .args(arguments)
        .env("FREEFLOW_ASSETS", assets)
        .status()?;
    ensure!(status.success(), "Freeflow exited with {status}");
    Ok(())
}
