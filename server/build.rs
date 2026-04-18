use std::process::Command;

use chrono::Local;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    let commit_short = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let build_time = Local::now().format("%Y%m%d%H%M%S").to_string();
    let build_time = if build_time.len() == 14 {
        build_time
    } else {
        "00000000000000".to_string()
    };

    println!("cargo:rustc-env=TODB_GIT_COMMIT_SHORT={commit_short}");
    println!("cargo:rustc-env=TODB_BUILD_TIME={build_time}");
}
