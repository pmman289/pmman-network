use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub(crate) const MAX_LOG_BYTES: u64 = 512 * 1024;
pub(crate) const MAX_LOG_FILES: usize = 3;
pub(crate) const MAX_BUNDLE_BYTES: usize = 768 * 1024;

static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> &'static Mutex<()> {
    LOG_LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(not(test))]
pub(crate) fn log_directory() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Pmman Network")
}

#[cfg(test)]
pub(crate) fn log_directory() -> PathBuf {
    // Unit fixtures must not rotate or append to a running user's diagnostics.
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY
        .get_or_init(|| {
            std::env::temp_dir().join(format!("pmman-service-unit-{}", uuid::Uuid::new_v4()))
        })
        .clone()
}

pub(crate) fn service_log_path() -> PathBuf {
    log_directory().join("service.log")
}

fn rotate(path: &Path) -> io::Result<()> {
    if !path.exists() || fs::metadata(path)?.len() < MAX_LOG_BYTES {
        return Ok(());
    }
    let oldest = path.with_extension(format!("log.{}", MAX_LOG_FILES));
    let _ = fs::remove_file(oldest);
    for index in (1..MAX_LOG_FILES).rev() {
        let source = path.with_extension(format!("log.{index}"));
        let target = path.with_extension(format!("log.{}", index + 1));
        if source.exists() {
            let _ = fs::rename(source, target);
        }
    }
    let first = path.with_extension("log.1");
    let _ = fs::rename(path, first);
    Ok(())
}

fn sanitize(message: &str) -> String {
    let mut value = message.replace(['\r', '\n'], " ");
    for marker in ["Bearer ", "password=", "token=", "private_key="] {
        loop {
            let Some(start) = value
                .to_ascii_lowercase()
                .find(&marker.to_ascii_lowercase())
            else {
                break;
            };
            let end = value[start + marker.len()..]
                .find(' ')
                .map(|offset| start + marker.len() + offset)
                .unwrap_or(value.len());
            value.replace_range(start + marker.len()..end, "<redacted>");
            break;
        }
    }
    value.chars().take(2_000).collect()
}

pub(crate) fn log(category: &str, message: &str) {
    let _guard = lock().lock().expect("diagnostic log lock poisoned");
    let directory = log_directory();
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = service_log_path();
    let _ = rotate(&path);
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let _ = writeln!(
        file,
        "{timestamp} [{}] {}",
        sanitize(category),
        sanitize(message)
    );
}

fn read_tail(path: &Path, remaining: usize) -> io::Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let size = file.metadata()?.len() as usize;
    let skip = size.saturating_sub(remaining);
    if skip > 0 {
        use std::io::Seek;
        file.seek(io::SeekFrom::Start(skip as u64))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if skip > 0 {
        if let Some(position) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=position);
        }
    }
    Ok(bytes)
}

pub(crate) fn bundle(summary: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(MAX_BUNDLE_BYTES);
    let manifest = format!(
        "Pmman Network diagnostic bundle\nformat=1\nmax_bytes={}\nsummary={}\n\n",
        MAX_BUNDLE_BYTES,
        sanitize(summary)
    );
    output.extend_from_slice(manifest.as_bytes());
    let mut remaining = MAX_BUNDLE_BYTES.saturating_sub(output.len());
    let mut paths = vec![("service.log", service_log_path())];
    paths.push(("service.log.1", log_directory().join("service.log.1")));
    paths.push(("service.log.2", log_directory().join("service.log.2")));
    paths.push(("update.log", log_directory().join("update.log")));
    for (name, path) in paths {
        if remaining < name.len() + 32 {
            break;
        }
        let section = format!("\n--- {name} ---\n");
        output.extend_from_slice(section.as_bytes());
        remaining = remaining.saturating_sub(section.len());
        let limit = remaining.min(192 * 1024);
        match read_tail(&path, limit) {
            Ok(bytes) => {
                let sanitized = String::from_utf8_lossy(&bytes)
                    .lines()
                    .map(sanitize)
                    .collect::<Vec<_>>()
                    .join("\n");
                let take = sanitized.len().min(remaining);
                output.extend_from_slice(&sanitized.as_bytes()[..take]);
                remaining = remaining.saturating_sub(take);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("读取诊断日志失败：{error}")),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_bundle_is_bounded_and_redacts_credentials() {
        let bundle = bundle("token=secret password=hunter2").unwrap();
        assert!(bundle.len() <= MAX_BUNDLE_BYTES);
        let text = String::from_utf8_lossy(&bundle);
        assert!(!text.contains("secret"));
        assert!(!text.contains("hunter2"));
    }
}
