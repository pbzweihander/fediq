use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=SKIP_TAILWINDCSS");

    if std::env::var_os("SKIP_TAILWINDCSS")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return;
    }

    println!("cargo:rerun-if-changed=templates");
    println!("cargo:rerun-if-changed=dist");

    let output = Command::new("tailwindcss")
        .args(["-i", "templates/base.css", "-o", "dist/index.css"])
        .output()
        .expect("failed to run tailwindcss CLI");
    if !output.status.success() {
        println!("cargo:warning=tailwindcss stdout:");
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            println!("cargo:warning=\t{line}");
        }
        println!("cargo:warning=tailwindcss stderr:");
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            println!("cargo:warning=\t{line}");
        }
        panic!("tailwindcss CLI failed")
    }
}
