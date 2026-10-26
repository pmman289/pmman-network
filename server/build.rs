fn main() {
    println!("cargo:rerun-if-env-changed=PMMAN_SERVER_VERSION");
    println!("cargo:rerun-if-changed=admin-dist");
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    assert!(
        root.join("admin-dist/index.html").is_file(),
        "Build the admin frontend first: cd admin && pnpm install --frozen-lockfile && pnpm build"
    );
}
