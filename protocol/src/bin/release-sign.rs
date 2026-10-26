// Offline release tooling. Private keys never travel to the master.
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer, SigningKey};
use pmman_protocol::release::Manifest;
use rand::RngCore;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("keygen") if args.len() == 4 => {
            if std::path::Path::new(&args[2]).exists() {
                return Err("key exists; refusing overwrite".into());
            }
            let mut bytes = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let key = SigningKey::from_bytes(&bytes);
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options
                .open(&args[2])?
                .write_all(STANDARD.encode(bytes).as_bytes())?;
            std::fs::write(&args[3], STANDARD.encode(key.verifying_key().as_bytes()))?;
        }
        Some("sign") if args.len() == 4 => {
            let raw = STANDARD.decode(std::fs::read_to_string(&args[2])?.trim())?;
            let key = SigningKey::from_bytes(&raw.try_into().map_err(|_| "key length")?);
            let mut manifest: Manifest = serde_json::from_slice(&std::fs::read(&args[3])?)?;
            manifest.validate(&manifest.channel)?;
            manifest.signature = STANDARD.encode(key.sign(&manifest.signing_bytes()?).to_bytes());
            std::fs::write(&args[3], serde_json::to_vec_pretty(&manifest)?)?;
        }
        _ => {
            return Err("usage: release-sign keygen PRIVATE PUBLIC | sign PRIVATE MANIFEST".into())
        }
    }
    println!("release_sign=passed");
    Ok(())
}
