use super::*;
use std::io::Read;

#[derive(Serialize)]
pub(super) struct Report {
    report_id: String,
    user_id: String,
    username: String,
    uploaded_at: DateTime<Utc>,
    size: u64,
}

fn base(state: &AppState) -> PathBuf {
    state
        .diagnostic_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("diagnostics"))
}

fn valid_user(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn valid_report(value: &str) -> bool {
    value
        .strip_prefix("diag_")
        .and_then(|id| Uuid::parse_str(id).ok())
        .is_some_and(|id| value == format!("diag_{id}"))
}

fn scan(base: &std::path::Path, users: &HashMap<String, String>) -> std::io::Result<Vec<Report>> {
    let directories = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    let mut reports = vec![];
    for directory in directories {
        let directory = directory?;
        if !directory.file_type()?.is_dir() {
            continue;
        }
        let user_id = directory.file_name().to_string_lossy().into_owned();
        if !valid_user(&user_id) {
            continue;
        }
        for file in fs::read_dir(directory.path())? {
            let file = file?;
            let file_type = match file.file_type() {
                Ok(kind) => kind,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if !file_type.is_file() {
                continue;
            }
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".log").filter(|id| valid_report(id)) else {
                continue;
            };
            let metadata = match file.metadata() {
                Ok(metadata) => metadata,
                // Upload retention can delete an old report during enumeration.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            reports.push(Report {
                report_id: id.into(),
                user_id: user_id.clone(),
                username: users
                    .get(&user_id)
                    .cloned()
                    .unwrap_or_else(|| user_id.clone()),
                uploaded_at: metadata.modified()?.into(),
                size: metadata.len(),
            });
        }
    }
    reports.sort_by(|a, b| {
        b.uploaded_at
            .cmp(&a.uploaded_at)
            .then_with(|| a.report_id.cmp(&b.report_id))
    });
    Ok(reports)
}

pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|s| release_error(s, "需要管理员权限"))?;
    let base = base(&state);
    let users = state
        .users
        .read()
        .expect("users poisoned")
        .values()
        .map(|user| (user.user_id.clone(), user.username.clone()))
        .collect();
    let reports = tokio::task::spawn_blocking(move || scan(&base, &users))
        .await
        .map_err(|_| release_error(StatusCode::INTERNAL_SERVER_ERROR, "读取诊断报告失败"))?
        .map_err(|_| release_error(StatusCode::INTERNAL_SERVER_ERROR, "读取诊断报告失败"))?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(reports)).into_response())
}

fn read(base: &std::path::Path, user: &str, report: &str) -> std::io::Result<Vec<u8>> {
    let directory = base.join(user);
    let path = directory.join(format!("{report}.log"));
    // Reports are regular files under a regular account directory, never links.
    if !fs::symlink_metadata(&directory)?.file_type().is_dir()
        || !fs::symlink_metadata(&path)?.file_type().is_file()
    {
        return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_CLIENT_DIAGNOSTIC_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CLIENT_DIAGNOSTIC_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "report too large",
        ));
    }
    Ok(bytes)
}

pub(super) async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((user_id, report_id)): Path<(String, String)>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    require_admin(&state, &headers).map_err(|s| release_error(s, "需要管理员权限"))?;
    if !valid_user(&user_id) || !valid_report(&report_id) {
        return Err(release_error(StatusCode::BAD_REQUEST, "报告编号无效"));
    }
    let base = base(&state);
    let name = report_id.clone();
    let bytes = tokio::task::spawn_blocking(move || read(&base, &user_id, &report_id))
        .await
        .map_err(|_| release_error(StatusCode::INTERNAL_SERVER_ERROR, "读取诊断报告失败"))?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                release_error(StatusCode::NOT_FOUND, "报告不存在或已被保留策略清理")
            } else {
                release_error(StatusCode::INTERNAL_SERVER_ERROR, "读取诊断报告失败")
            }
        })?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}.log\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    #[test]
    fn report_paths_reject_traversal_and_noncanonical_ids() {
        for value in ["../u1", "u/1", "u\\1", "", ".", ".."] {
            assert!(!valid_user(value));
        }
        for value in [
            "diag_../users",
            "diag_not-a-uuid",
            "diag_32cc902b177246deb4f11c452eabd277",
        ] {
            assert!(!valid_report(value));
        }
        assert!(valid_report("diag_32cc902b-1772-46de-b4f1-1c452eabd277"));
    }

    #[tokio::test]
    async fn admin_can_list_and_download_existing_uploads_only() {
        let directory = env::temp_dir().join(format!("pmman-diagnostics-{}", Uuid::new_v4()));
        fs::create_dir_all(directory.join("u1")).unwrap();
        let id = format!("diag_{}", Uuid::new_v4());
        fs::write(
            directory.join("u1").join(format!("{id}.log")),
            b"example diagnostic\n",
        )
        .unwrap();
        fs::write(directory.join("u1/other.txt"), b"not a report").unwrap();
        let mut state = crate::tests::test_state();
        state.diagnostic_dir = Some(directory.clone());
        let router = app(state);
        let user = crate::tests::login_token(&router, "u1", "userpw").await;
        let admin = crate::tests::login_token(&router, "admin", "adminpw").await;
        let url = format!("/v1/admin/diagnostics/u1/{id}");
        for endpoint in ["/v1/admin/diagnostics", url.as_str()] {
            for (token, status) in [
                ("", StatusCode::UNAUTHORIZED),
                (user.as_str(), StatusCode::FORBIDDEN),
            ] {
                let result = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(endpoint)
                            .header("Authorization", format!("Bearer {token}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(result.status(), status);
            }
        }
        let listed = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/admin/diagnostics")
                    .header("Authorization", format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(listed.into_body(), MAX_CLIENT_DIAGNOSTIC_BYTES)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["report_id"], id);
        assert_eq!(listed[0]["username"], "u1");
        let downloaded = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&url)
                    .header("Authorization", format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(downloaded.status(), StatusCode::OK);
        assert_eq!(downloaded.headers()[header::CACHE_CONTROL], "no-store");
        assert!(downloaded.headers()[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .contains(&format!("{id}.log")));
        assert_eq!(
            axum::body::to_bytes(downloaded.into_body(), MAX_CLIENT_DIAGNOSTIC_BYTES)
                .await
                .unwrap()
                .as_ref(),
            b"example diagnostic\n"
        );
        fs::remove_file(directory.join("u1").join(format!("{id}.log"))).unwrap();
        let missing = router
            .oneshot(
                Request::builder()
                    .uri(&url)
                    .header("Authorization", format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        fs::remove_dir_all(directory).unwrap();
    }
}
