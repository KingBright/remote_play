//! Stable startup error codes: never include profile paths, metadata or secrets.
use remote_core::mesh::MeshStoreError;
use std::path::Path;

pub(crate) fn profile_error_code(error: &MeshStoreError, storage: &Path) -> String {
    let role = |path: &Path| {
        if path == storage {
            "app-files"
        } else if path.starts_with(storage) {
            "app-private"
        } else {
            "system-ancestor"
        }
    };
    match error {
        MeshStoreError::Io { path, source } => format!(
            "profile-io:{}:os={}",
            role(path),
            source.raw_os_error().unwrap_or(0)
        ),
        MeshStoreError::UnsafeProfile { path, message } => {
            let kind = if message.contains("symlink") {
                "symlink"
            } else if message.contains("not private") {
                "permissions"
            } else if message.contains("owner") || message.contains("owned") {
                "ownership"
            } else if message.contains("ordinary directory") {
                "directory"
            } else {
                "policy"
            };
            format!("profile-unsafe:{}:{}", role(path), kind)
        }
        MeshStoreError::MissingNetworkSecret => "profile-missing-secret".into(),
        MeshStoreError::InvalidMetadata(_) => "profile-invalid-metadata".into(),
        MeshStoreError::InvalidMeshConfig(_) => "profile-invalid-config".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_errors_do_not_disclose_profile_paths_or_contents() {
        let private = Path::new("/private/sentinel-owner/files");
        let error = MeshStoreError::Io {
            path: private.join("secret-value"),
            source: std::io::Error::from_raw_os_error(13),
        };
        assert_eq!(
            profile_error_code(&error, private),
            "profile-io:app-private:os=13"
        );
        assert_eq!(
            profile_error_code(
                &MeshStoreError::InvalidMetadata("sentinel-secret".into()),
                private
            ),
            "profile-invalid-metadata"
        );
    }
    #[test]
    fn inaccessible_os_ancestors_are_distinguished_without_paths() {
        let error = MeshStoreError::Io {
            path: "/data/user".into(),
            source: std::io::Error::from_raw_os_error(13),
        };
        assert_eq!(
            profile_error_code(&error, Path::new("/data/user/0/app/files")),
            "profile-io:system-ancestor:os=13"
        );
    }
}
