// Build script for nodelite-server: triggers Vite build for the Vue SPA.
//
// This script runs before compiling the Rust code and ensures the frontend
// assets are built and ready to be embedded into the binary.

use std::path::Path;
use std::process::Command;

fn main() {
    // Allow skipping web build for backend-only iteration
    if std::env::var("NODELITE_SKIP_WEB_BUILD").is_ok() {
        // If web/dist/ exists, reuse it; otherwise emit a warning but don't fail
        if !Path::new("web/dist/index.html").exists() {
            println!(
                "cargo:warning=NODELITE_SKIP_WEB_BUILD set but web/dist/ is empty; \
                 server will serve a placeholder. Run `pnpm --dir web build` first."
            );
        }
        return;
    }

    // Tell cargo to rerun this script if any of these paths change
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/pnpm-lock.yaml");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/tsconfig.json");

    // Check if pnpm is available
    let pnpm = which::which("pnpm").unwrap_or_else(|_| {
        eprintln!();
        eprintln!("===========================================================");
        eprintln!(" nodelite-server requires pnpm to build frontend assets.");
        eprintln!();
        eprintln!(" Installation options:");
        eprintln!("   • brew install pnpm");
        eprintln!("   • npm install -g pnpm");
        eprintln!("   • curl -fsSL https://get.pnpm.io/install.sh | sh");
        eprintln!();
        eprintln!(" To skip frontend build (backend-only iteration):");
        eprintln!("   NODELITE_SKIP_WEB_BUILD=1 cargo build");
        eprintln!("===========================================================");
        std::process::exit(1);
    });

    // Run pnpm install
    let output = Command::new(&pnpm)
        .args(["--dir", "web", "install", "--frozen-lockfile"])
        .output()
        .expect("failed to spawn pnpm install");
    if !output.status.success() {
        eprintln!("pnpm install stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("pnpm install stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("pnpm install failed");
    }

    // Run pnpm build
    let output = Command::new(&pnpm)
        .args(["--dir", "web", "build"])
        .output()
        .expect("failed to spawn pnpm build");
    if !output.status.success() {
        eprintln!("pnpm build stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("pnpm build stderr: {}", String::from_utf8_lossy(&output.stderr));
        panic!("pnpm build failed");
    }
}
