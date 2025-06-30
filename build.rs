use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Check if we're in CI/CD environment to increment patch version
    let in_ci = env::var("CI").is_ok() || env::var("GITHUB_ACTIONS").is_ok();
    
    let final_version = if in_ci {
        // In CI: increment patch version
        let base_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string());
        increment_patch_version(&base_version)
    } else {
        // Local development: use version from Cargo.toml
        env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.1.0".to_string())
    };
    
    // Get the build number file path for local builds
    let build_number_file = Path::new("build_number.txt");
    
    // Read current build number or start at 1
    let build_number = if build_number_file.exists() {
        fs::read_to_string(build_number_file)
            .unwrap_or_else(|_| "1".to_string())
            .trim()
            .parse::<u32>()
            .unwrap_or(1)
    } else {
        1
    };
    
    // Increment build number for local builds
    let new_build_number = if !in_ci {
        let incremented = build_number + 1;
        // Write new build number back to file
        fs::write(build_number_file, incremented.to_string())
            .expect("Failed to write build number");
        incremented
    } else {
        build_number
    };
    
    // Get git commit hash if available
    let git_hash = std::process::Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    
    // Get build timestamp
    let build_time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    
    // Get current branch if available
    let git_branch = std::process::Command::new("git")
        .args(&["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    
    // Make build information available to the program
    println!("cargo:rustc-env=FINAL_VERSION={}", final_version);
    println!("cargo:rustc-env=BUILD_NUMBER={}", new_build_number);
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);
    println!("cargo:rustc-env=GIT_BRANCH={}", git_branch);
    println!("cargo:rustc-env=VERSION_WITH_GIT={}+{}", final_version, git_hash);
    
    // Tell cargo to rerun this script if git changes
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/");
}

fn increment_patch_version(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return format!("{}.1", version); // fallback if not semver
    }
    
    let major = parts[0];
    let minor = parts[1];
    let patch: u32 = parts[2].parse().unwrap_or(0);
    
    format!("{}.{}.{}", major, minor, patch + 1)
}