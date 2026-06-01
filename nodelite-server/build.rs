// Build script for nodelite-server: triggers Vite build for the Vue SPA.
//
// This script runs before compiling the Rust code and ensures the frontend
// assets are built and ready to be embedded into the binary.

use std::path::Path;
use std::process::Command;

fn main() {
    // Toggling skip mode must re-run this script; otherwise Cargo caches the
    // previous build-script decision and silently reuses (or skips) the build.
    println!("cargo:rerun-if-env-changed=NODELITE_SKIP_WEB_BUILD");

    // Allow skipping the web build for backend-only iteration / CI jobs that
    // reuse a prebuilt web/dist. `web_assets.rs` embeds web/dist at compile time
    // via include_dir!, which panics if the directory is missing — so a skip
    // with no prebuilt dist cannot "serve a placeholder"; fail loudly here
    // instead of letting it surface as an opaque macro panic later.
    if std::env::var("NODELITE_SKIP_WEB_BUILD").is_ok() {
        if !Path::new("web/dist/index.html").exists() {
            eprintln!();
            eprintln!("===========================================================");
            eprintln!(" NODELITE_SKIP_WEB_BUILD is set but web/dist/index.html");
            eprintln!(" is missing.");
            eprintln!();
            eprintln!(" web/dist is gitignored and embedded at compile time via");
            eprintln!(" include_dir!, so the build cannot proceed without it.");
            eprintln!();
            eprintln!(" Build the frontend first:");
            eprintln!("   pnpm --dir web build");
            eprintln!(" or unset NODELITE_SKIP_WEB_BUILD to build it automatically.");
            eprintln!("===========================================================");
            std::process::exit(1);
        }
        return;
    }

    // Tell cargo to rerun this script if any of these paths change. web/public
    // is copied verbatim into web/dist by Vite (verify-2fa.html, ui-i18n.json,
    // logos), so edits there must invalidate the embedded assets too.
    println!("cargo:rerun-if-changed=web/src");
    println!("cargo:rerun-if-changed=web/public");
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
        eprintln!(
            "pnpm install stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "pnpm install stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("pnpm install failed");
    }

    // Run pnpm build
    let output = Command::new(&pnpm)
        .args(["--dir", "web", "build"])
        .output()
        .expect("failed to spawn pnpm build");
    if !output.status.success() {
        eprintln!(
            "pnpm build stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        eprintln!(
            "pnpm build stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        panic!("pnpm build failed");
    }
}
