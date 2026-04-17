use std::{
    env,
    ffi::OsStr,
    fs,
    io,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    // Re-run when UI sources change.
    let ui_dir = PathBuf::from("ui");
    println!("cargo:rerun-if-changed=ui/package.json");
    println!("cargo:rerun-if-changed=ui/package-lock.json");
    println!("cargo:rerun-if-changed=ui/vite.config.ts");
    println!("cargo:rerun-if-changed=ui/index.html");
    println!("cargo:rerun-if-changed=ui/tsconfig.json");
    println!("cargo:rerun-if-changed=ui/tsconfig.app.json");
    println!("cargo:rerun-if-changed=ui/tsconfig.node.json");
    println!("cargo:rerun-if-changed=ui/src");

    // Allow opting out for environments without Node (e.g. minimal CI),
    // while keeping default behavior "always build UI".
    if env::var_os("USBIP_SERVER_SKIP_UI_BUILD").is_some() {
        println!("cargo:warning=Skipping UI build (USBIP_SERVER_SKIP_UI_BUILD is set)");
        return;
    }

    ensure_cmd_exists("npm");

    // `npm ci` prefers a lockfile. If it's missing, guide the user.
    if !ui_dir.join("package-lock.json").exists() {
        panic!(
            "usbip-server UI missing package-lock.json. Run `npm install` in {:?} to generate it.",
            ui_dir
        );
    }

    run(&ui_dir, "npm", &["ci"]);
    run(&ui_dir, "npm", &["run", "build"]);

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR must be set"));
    let dst = out_dir.join("ui-dist");
    let src = ui_dir.join("dist");
    if !src.exists() {
        panic!("Vite build did not produce {:?}. Did `npm run build` succeed?", src);
    }

    let _ = fs::remove_dir_all(&dst);
    copy_dir_all(&src, &dst).expect("copy ui dist to OUT_DIR");
}

fn ensure_cmd_exists(cmd: &str) {
    let ok = Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();
    if !ok {
        panic!(
            "Required command `{}` not found. Install Node.js/npm, or set USBIP_SERVER_SKIP_UI_BUILD=1 to skip.",
            cmd
        );
    }
}

fn run(cwd: &Path, program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {program} {args:?}: {e}"));
    if !status.success() {
        panic!("{program} {args:?} failed with exit code {status}");
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_file() {
            copy_file(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    // Best-effort: preserve executable bit if any (not expected for web assets).
    let mut tmp = to.to_owned();
    tmp.set_extension(format!(
        "{}.tmp",
        to.extension().and_then(OsStr::to_str).unwrap_or("bin")
    ));
    fs::copy(from, &tmp)?;
    fs::rename(tmp, to)?;
    Ok(())
}

