use std::process::Command;

/// Embarque le commit git court dans le binaire pour `cs --version`.
///
/// Priorité à la variable `CS_GIT_COMMIT` fournie par l'environnement
/// (build Nix : le sandbox n'a pas accès au `.git`, le flake l'injecte) ;
/// sinon on interroge `git` (build cargo local) ; sinon `"unknown"`.
fn main() {
    let commit = std::env::var("CS_GIT_COMMIT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=CS_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=CS_GIT_COMMIT");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/main");
}
