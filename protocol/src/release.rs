use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub product: String,
    pub channel: String,
    pub platform: String,
    pub arch: String,
    pub version: String,
    pub min_supported_version: String,
    pub mandatory_after: Option<String>,
    pub installer_file: String,
    pub size: u64,
    pub sha256: String,
    pub signature: String,
    pub published_at: String,
    pub release_notes: Vec<String>,
}

impl Manifest {
    // serde_json's default Map uses sorted keys. This encoding is shared by
    // the offline signer and verifier; never sign arbitrary HTTP response text.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, String> {
        let mut value = serde_json::to_value(self).map_err(|e| e.to_string())?;
        value.as_object_mut().unwrap().remove("signature");
        serde_json::to_vec(&value).map_err(|e| e.to_string())
    }

    pub fn validate(&self, channel: &str) -> Result<(), String> {
        if self.product != "pmman-network-client"
            || self.channel != channel
            || !matches!(channel, "stable" | "beta")
            || self.platform != "windows"
            || self.arch != "x64"
            || self.size == 0
            || self.size > 64 * 1024 * 1024
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|v| v.is_ascii_hexdigit())
            || !self.installer_file.ends_with(".exe")
            || self
                .installer_file
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
            || !self
                .installer_file
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
        {
            return Err("更新清单产品、渠道、平台、文件名或大小无效".into());
        }
        let version = semver::Version::parse(&self.version).map_err(|_| "更新版本无效")?;
        let minimum =
            semver::Version::parse(&self.min_supported_version).map_err(|_| "最低版本无效")?;
        if minimum > version {
            return Err("最低版本不能高于发布版本".into());
        }
        Ok(())
    }

    pub fn verify(&self, key: &str, channel: &str) -> Result<(), String> {
        self.validate(channel)?;
        let raw = STANDARD
            .decode(key.trim())
            .map_err(|_| "发布公钥格式无效")?;
        let raw: [u8; 32] = raw.try_into().map_err(|_| "发布公钥长度无效")?;
        let key = VerifyingKey::from_bytes(&raw).map_err(|_| "发布公钥无效")?;
        let sig = STANDARD
            .decode(&self.signature)
            .map_err(|_| "发布签名格式无效")?;
        let sig = Signature::from_slice(&sig).map_err(|_| "发布签名长度无效")?;
        key.verify_strict(&self.signing_bytes()?, &sig)
            .map_err(|_| "客户端更新签名校验失败".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    #[test]
    fn signature_binds_all_fields_and_rejects_tampering() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut m = Manifest {
            product: "pmman-network-client".into(),
            channel: "stable".into(),
            platform: "windows".into(),
            arch: "x64".into(),
            version: "0.2.0".into(),
            min_supported_version: "0.0.1-alpha".into(),
            mandatory_after: None,
            installer_file: "0.2.0/update.exe".into(),
            size: 100,
            sha256: "a".repeat(64),
            signature: String::new(),
            published_at: "2026-10-06T00:00:00Z".into(),
            release_notes: vec![],
        };
        m.signature = STANDARD.encode(key.sign(&m.signing_bytes().unwrap()).to_bytes());
        let public = STANDARD.encode(key.verifying_key().as_bytes());
        m.verify(&public, "stable").unwrap();
        m.version = "0.3.0".into();
        assert!(m.verify(&public, "stable").is_err());
        m.installer_file = "../secret.exe".into();
        assert!(m.validate("stable").is_err());
    }
}
