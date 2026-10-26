fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 5 && args[1] == "--sign-manifest" {
        if let Err(error) = sign_manifest(&args[2], &args[3], &args[4]) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() == 4 && args[1] == "--copy-result" {
        let temporary = format!("{}.tmp", args[3]);
        if std::fs::copy(&args[2], &temporary).is_err()
            || std::fs::rename(&temporary, &args[3]).is_err()
        {
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
        return;
    }
    // The privileged GuestSetup phase performs the update scenario. This
    // unprivileged continuation gives the broker a stable process to observe
    // while it evaluates the result marker.
    std::thread::sleep(std::time::Duration::from_secs(2));
}

fn sign_manifest(key_path: &str, package_path: &str, output_path: &str) -> Result<(), String> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signer, SigningKey};
    use pmman_protocol::release::Manifest;
    use sha2::{Digest, Sha256};

    let key_bytes = STANDARD
        .decode(std::fs::read_to_string(key_path).map_err(|e| e.to_string())?.trim())
        .map_err(|e| e.to_string())?;
    let key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| "client signing key must be 32 bytes".to_string())?;
    let package = std::fs::read(package_path).map_err(|e| e.to_string())?;
    let manifest = Manifest {
        product: "pmman-network-client".into(),
        channel: "stable".into(),
        platform: "windows".into(),
        arch: "x64".into(),
        version: "0.1.0-alpha".into(),
        min_supported_version: "0.0.6-alpha".into(),
        mandatory_after: None,
        installer_file: "0.1.0-alpha/pmman-network-0.1.0-alpha.exe".into(),
        size: package.len() as u64,
        sha256: format!("{:x}", Sha256::digest(package)),
        signature: String::new(),
        published_at: "2026-10-06T06:00:00Z".into(),
        release_notes: vec!["修复客户端静默更新流程".into()],
    };
    let signing_key = SigningKey::from_bytes(&key);
    let mut signed = manifest;
    signed.signature = STANDARD.encode(signing_key.sign(&signed.signing_bytes()?).to_bytes());
    std::fs::write(
        output_path,
        serde_json::to_vec_pretty(&signed).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
