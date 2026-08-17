//! Versioned model manifests and a checksum-verified local cache.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{ModelDownloadPolicy, OcrOptions};

/// Environment variable overriding the default local model cache.
pub const MODEL_CACHE_ENV: &str = "PDF_INSPECTOR_MODEL_CACHE";

/// Role of an artifact within a local model set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum ModelArtifactKind {
    /// Text detection ONNX graph.
    TextDetection,
    /// Text recognition ONNX graph.
    TextRecognition,
    /// Recognition character dictionary.
    CharacterDictionary,
    /// Learned document-layout ONNX graph.
    Layout,
}

/// One immutable file in a model manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelArtifact {
    /// Artifact role.
    pub kind: ModelArtifactKind,
    /// Cache filename without directory components.
    pub filename: &'static str,
    /// Canonical HTTPS download location.
    pub url: &'static str,
    /// Lowercase SHA-256 digest.
    pub sha256: &'static str,
    /// Exact expected file size.
    pub size: u64,
}

/// Versioned set of files required by one model configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Stable model-set identifier.
    pub id: &'static str,
    /// Immutable upstream artifact revision.
    pub revision: &'static str,
    /// Required artifacts.
    pub artifacts: &'static [ModelArtifact],
}

const PP_OCR_V6_SMALL_ARTIFACTS: &[ModelArtifact] = &[
    ModelArtifact {
        kind: ModelArtifactKind::TextDetection,
        filename: "pp-ocrv6_small_det.onnx",
        url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/pp-ocrv6_small_det.onnx",
        sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
        size: 9_880_512,
    },
    ModelArtifact {
        kind: ModelArtifactKind::TextRecognition,
        filename: "pp-ocrv6_small_rec.onnx",
        url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/pp-ocrv6_small_rec.onnx",
        sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
        size: 21_159_378,
    },
    ModelArtifact {
        kind: ModelArtifactKind::CharacterDictionary,
        filename: "ppocrv6_dict.txt",
        url: "https://github.com/GreatV/oar-ocr/releases/download/v0.7.0/ppocrv6_dict.txt",
        sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d",
        size: 74_947,
    },
];

/// Pinned PP-OCRv6 Small detection/recognition model set.
///
/// The artifact hashes match the registry shipped by `oar-ocr-core` 0.9.1;
/// the revision identifies the upstream release that owns the files.
pub const PP_OCR_V6_SMALL: ModelManifest = ModelManifest {
    schema_version: 1,
    id: "pp-ocrv6-small",
    revision: "oar-ocr-v0.7.0",
    artifacts: PP_OCR_V6_SMALL_ARTIFACTS,
};

/// Resolved, verified filesystem paths for one model manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPaths {
    manifest_id: String,
    revision: String,
    artifacts: BTreeMap<ModelArtifactKind, PathBuf>,
}

impl ModelPaths {
    /// Stable model-set identifier.
    pub fn manifest_id(&self) -> &str {
        &self.manifest_id
    }

    /// Immutable artifact revision.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Verified path for an artifact role.
    pub fn get(&self, kind: ModelArtifactKind) -> Option<&Path> {
        self.artifacts.get(&kind).map(PathBuf::as_path)
    }

    /// Iterates over verified artifact paths.
    pub fn iter(&self) -> impl Iterator<Item = (ModelArtifactKind, &Path)> {
        self.artifacts
            .iter()
            .map(|(&kind, path)| (kind, path.as_path()))
    }
}

/// Checksum-verified model cache with an optional offline directory override.
///
/// This type never accesses the network. [`resolve`](Self::resolve) verifies a
/// warm cache or explicit directory, and [`install`](Self::install) atomically
/// installs bytes supplied by a higher-level downloader. Keeping acquisition
/// separate makes offline behavior enforceable and straightforward to test.
#[derive(Debug, Clone)]
pub struct ModelStore {
    cache_root: PathBuf,
    override_root: Option<PathBuf>,
}

/// Streaming source for a pinned model artifact.
///
/// Implementations may use HTTP, an object store, or an application-owned
/// package manager. [`ModelStore`] remains responsible for locking, atomic
/// installation, exact size validation, and SHA-256 verification.
pub trait ModelDownloader: Send + Sync {
    /// Downloader-specific failure type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Opens a streaming reader for one manifest artifact.
    fn open(&self, artifact: &ModelArtifact) -> Result<Box<dyn Read + Send>, Self::Error>;
}

impl ModelStore {
    /// Creates a model store rooted at an explicit cache directory.
    pub fn new(cache_root: impl Into<PathBuf>) -> Self {
        Self {
            cache_root: cache_root.into(),
            override_root: None,
        }
    }

    /// Builds a store from OCR options and the platform cache directory.
    ///
    /// `PDF_INSPECTOR_MODEL_CACHE` overrides the platform default. An explicit
    /// [`OcrOptions::model_directory`] replaces the managed cache at resolve
    /// time so offline packaging is deterministic.
    pub fn from_options(options: &OcrOptions) -> Result<Self, ModelStoreError> {
        let cache_root = match std::env::var_os(MODEL_CACHE_ENV) {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => dirs::cache_dir()
                .ok_or(ModelStoreError::CacheDirectoryUnavailable)?
                .join("pdf-inspector")
                .join("models"),
        };
        Ok(Self {
            cache_root,
            override_root: options.model_directory.clone(),
        })
    }

    /// Checks an explicit offline model directory before the managed cache.
    pub fn override_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.override_root = Some(root.into());
        self
    }

    /// Managed cache root.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Effective directory containing one manifest's artifacts.
    pub(crate) fn model_root(&self, manifest: &ModelManifest) -> PathBuf {
        self.override_root
            .clone()
            .unwrap_or_else(|| self.manifest_cache_root(manifest))
    }

    /// Validates and resolves every required artifact.
    pub fn resolve(&self, manifest: &ModelManifest) -> Result<ModelPaths, ModelStoreError> {
        validate_manifest(manifest)?;
        let root = self.model_root(manifest);

        let mut artifacts = BTreeMap::new();
        for artifact in manifest.artifacts {
            let path = root.join(artifact.filename);
            verify_artifact(&path, artifact)?;
            artifacts.insert(artifact.kind, path);
        }
        Ok(ModelPaths {
            manifest_id: manifest.id.to_string(),
            revision: manifest.revision.to_string(),
            artifacts,
        })
    }

    /// Resolves a complete model set, fetching only missing or invalid managed
    /// cache artifacts when policy permits.
    ///
    /// Explicit [`OcrOptions::model_directory`] overrides are never mutated or
    /// supplemented from the network. This method also avoids all downloader
    /// calls when the cache is already valid or downloads are offline.
    pub fn resolve_or_download<D: ModelDownloader>(
        &self,
        manifest: &ModelManifest,
        policy: ModelDownloadPolicy,
        downloader: &D,
    ) -> Result<ModelPaths, ModelAcquireError<D::Error>> {
        validate_manifest(manifest).map_err(ModelAcquireError::Store)?;
        match self.resolve(manifest) {
            Ok(paths) => return Ok(paths),
            Err(source) if self.override_root.is_some() => {
                return Err(ModelAcquireError::ExplicitDirectoryIncomplete { source });
            }
            Err(source) if !source.permits_download_recovery() => {
                return Err(ModelAcquireError::Store(source));
            }
            Err(source) if policy == ModelDownloadPolicy::Offline => {
                return Err(ModelAcquireError::DownloadsDisabled { source });
            }
            Err(_) => {}
        }

        let root = self.manifest_cache_root(manifest);
        for artifact in manifest.artifacts {
            let _lock = lock_artifact(&root, artifact).map_err(ModelAcquireError::Store)?;
            match verify_artifact(&root.join(artifact.filename), artifact) {
                Ok(()) => continue,
                Err(source) if source.permits_download_recovery() => {}
                Err(source) => return Err(ModelAcquireError::Store(source)),
            }
            let reader =
                downloader
                    .open(artifact)
                    .map_err(|source| ModelAcquireError::Download {
                        kind: artifact.kind,
                        url: artifact.url,
                        source,
                    })?;
            install_locked(&root, artifact, reader).map_err(ModelAcquireError::Store)?;
        }
        self.resolve(manifest).map_err(ModelAcquireError::Store)
    }

    /// Atomically installs one artifact from a reader after validating its
    /// exact size and SHA-256 digest.
    ///
    /// A cross-process file lock serializes installs of the same artifact.
    /// Already-valid cached files are reused without consuming the reader.
    pub fn install(
        &self,
        manifest: &ModelManifest,
        kind: ModelArtifactKind,
        mut reader: impl Read,
    ) -> Result<PathBuf, ModelStoreError> {
        validate_manifest(manifest)?;
        let artifact = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == kind)
            .ok_or(ModelStoreError::ArtifactNotInManifest { kind })?;
        let root = self.manifest_cache_root(manifest);
        let _lock = lock_artifact(&root, artifact)?;
        install_locked(&root, artifact, &mut reader)
    }

    fn manifest_cache_root(&self, manifest: &ModelManifest) -> PathBuf {
        self.cache_root.join(manifest.id).join(manifest.revision)
    }
}

/// Failures while resolving or lazily acquiring a model set.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelAcquireError<E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    /// Manifest validation, cache verification, or installation failed.
    #[error(transparent)]
    Store(#[from] ModelStoreError),
    /// A managed-cache artifact could not be downloaded.
    #[error("failed to download {kind:?} from {url}: {source}")]
    Download {
        /// Artifact role.
        kind: ModelArtifactKind,
        /// Pinned source URL.
        url: &'static str,
        /// Downloader failure.
        #[source]
        source: E,
    },
    /// Offline policy prevented acquisition of an unavailable artifact.
    #[error("model artifacts are unavailable and downloads are disabled: {source}")]
    DownloadsDisabled {
        /// Original verification failure.
        #[source]
        source: ModelStoreError,
    },
    /// An explicit package-managed directory was incomplete or invalid.
    #[error("explicit model directory is incomplete or invalid: {source}")]
    ExplicitDirectoryIncomplete {
        /// Original verification failure.
        #[source]
        source: ModelStoreError,
    },
}

/// Failures while validating or installing model artifacts.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ModelStoreError {
    /// No platform cache location is available.
    #[error("no platform cache directory is available; set {MODEL_CACHE_ENV}")]
    CacheDirectoryUnavailable,
    /// The static/custom manifest is malformed.
    #[error("invalid model manifest: {0}")]
    InvalidManifest(String),
    /// A requested artifact role is not in the manifest.
    #[error("artifact role {kind:?} is not present in the model manifest")]
    ArtifactNotInManifest {
        /// Missing role.
        kind: ModelArtifactKind,
    },
    /// A required artifact is not installed.
    #[error("model artifact is missing: {path}")]
    MissingArtifact {
        /// Expected local path.
        path: PathBuf,
        /// Canonical download URL for an allowed higher-level fetcher.
        download_url: &'static str,
    },
    /// Artifact byte size differs from the manifest.
    #[error("model artifact {path} has {actual} bytes; expected {expected}")]
    SizeMismatch {
        /// Artifact path.
        path: PathBuf,
        /// Expected byte count.
        expected: u64,
        /// Actual byte count.
        actual: u64,
    },
    /// Artifact digest differs from the manifest.
    #[error("model artifact checksum mismatch at {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Artifact path.
        path: PathBuf,
        /// Expected lowercase SHA-256.
        expected: &'static str,
        /// Actual lowercase SHA-256.
        actual: String,
    },
    /// Filesystem or stream I/O failed.
    #[error("model cache I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
}

impl ModelStoreError {
    fn permits_download_recovery(&self) -> bool {
        matches!(
            self,
            Self::MissingArtifact { .. }
                | Self::SizeMismatch { .. }
                | Self::ChecksumMismatch { .. }
        )
    }
}

fn validate_manifest(manifest: &ModelManifest) -> Result<(), ModelStoreError> {
    if manifest.schema_version != 1 {
        return Err(ModelStoreError::InvalidManifest(format!(
            "unsupported schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.id.is_empty() || manifest.revision.is_empty() || manifest.artifacts.is_empty() {
        return Err(ModelStoreError::InvalidManifest(
            "id, revision, and artifacts must be non-empty".to_string(),
        ));
    }
    for (field, value) in [("id", manifest.id), ("revision", manifest.revision)] {
        if !is_single_normal_path_component(value) {
            return Err(ModelStoreError::InvalidManifest(format!(
                "{field} must be a single path component: {value}"
            )));
        }
    }

    let mut kinds = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for artifact in manifest.artifacts {
        if !is_single_normal_path_component(artifact.filename) {
            return Err(ModelStoreError::InvalidManifest(format!(
                "artifact filename must be a single path component: {}",
                artifact.filename
            )));
        }
        if artifact.size == 0 {
            return Err(ModelStoreError::InvalidManifest(format!(
                "artifact {} has zero size",
                artifact.filename
            )));
        }
        if artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ModelStoreError::InvalidManifest(format!(
                "artifact {} has an invalid SHA-256",
                artifact.filename
            )));
        }
        if !artifact.url.starts_with("https://") {
            return Err(ModelStoreError::InvalidManifest(format!(
                "artifact {} must use HTTPS",
                artifact.filename
            )));
        }
        if !kinds.insert(artifact.kind) || !filenames.insert(artifact.filename) {
            return Err(ModelStoreError::InvalidManifest(format!(
                "artifact {} duplicates a kind or filename",
                artifact.filename
            )));
        }
    }
    Ok(())
}

fn is_single_normal_path_component(value: &str) -> bool {
    if value.is_empty() || Path::new(value).file_name() != Some(OsStr::new(value)) {
        return false;
    }
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn lock_artifact(root: &Path, artifact: &ModelArtifact) -> Result<File, ModelStoreError> {
    fs::create_dir_all(root).map_err(|source| ModelStoreError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let lock_path = root.join(format!(".{}.lock", artifact.filename));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|source| ModelStoreError::Io {
            path: lock_path.clone(),
            source,
        })?;
    FileExt::lock_exclusive(&lock).map_err(|source| ModelStoreError::Io {
        path: lock_path,
        source,
    })?;
    Ok(lock)
}

fn install_locked(
    root: &Path,
    artifact: &ModelArtifact,
    mut reader: impl Read,
) -> Result<PathBuf, ModelStoreError> {
    let target = root.join(artifact.filename);
    if verify_artifact(&target, artifact).is_ok() {
        return Ok(target);
    }

    sweep_stale_install_files(root, artifact.filename)?;
    let (temporary, mut output) = create_temporary_file(root, artifact.filename)?;
    let result = (|| {
        let mut limited = reader.by_ref().take(artifact.size.saturating_add(1));
        let (size, digest) =
            copy_and_hash(&mut limited, &mut output).map_err(|source| ModelStoreError::Io {
                path: temporary.clone(),
                source,
            })?;
        output.sync_all().map_err(|source| ModelStoreError::Io {
            path: temporary.clone(),
            source,
        })?;
        validate_size_and_hash(artifact, size, &digest, &temporary)?;

        replace_file_atomic(&temporary, &target).map_err(|source| ModelStoreError::Io {
            path: target.clone(),
            source,
        })?;
        Ok(target.clone())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn sweep_stale_install_files(root: &Path, filename: &str) -> Result<(), ModelStoreError> {
    let prefix = format!(".{filename}.");
    for entry in fs::read_dir(root).map_err(|source| ModelStoreError::Io {
        path: root.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| ModelStoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix) && name.ends_with(".part") {
            let path = entry.path();
            fs::remove_file(&path).map_err(|source| ModelStoreError::Io { path, source })?;
        }
    }
    Ok(())
}

fn create_temporary_file(root: &Path, filename: &str) -> Result<(PathBuf, File), ModelStoreError> {
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    for _ in 0..16 {
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!(
            ".{filename}.{}.{}.{}.part",
            std::process::id(),
            timestamp,
            sequence
        ));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(ModelStoreError::Io { path, source }),
        }
    }
    let path = root.join(format!(".{filename}.part"));
    Err(ModelStoreError::Io {
        path,
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique model install file",
        ),
    })
}

#[cfg(not(windows))]
fn replace_file_atomic(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file_atomic(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn verify_artifact(path: &Path, artifact: &ModelArtifact) -> Result<(), ModelStoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ModelStoreError::MissingArtifact {
                path: path.to_path_buf(),
                download_url: artifact.url,
            });
        }
        Err(source) => {
            return Err(ModelStoreError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if metadata.len() != artifact.size {
        return Err(ModelStoreError::SizeMismatch {
            path: path.to_path_buf(),
            expected: artifact.size,
            actual: metadata.len(),
        });
    }

    let mut file = File::open(path).map_err(|source| ModelStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut DigestWriter(&mut hasher)).map_err(|source| ModelStoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let digest = digest_hex(hasher.finalize());
    validate_size_and_hash(artifact, metadata.len(), &digest, path)
}

fn copy_and_hash(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut size = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("model artifact size overflow"))?;
    }
    Ok((size, digest_hex(hasher.finalize())))
}

fn digest_hex(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = digest.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn validate_size_and_hash(
    artifact: &ModelArtifact,
    size: u64,
    digest: &str,
    path: &Path,
) -> Result<(), ModelStoreError> {
    if size != artifact.size {
        return Err(ModelStoreError::SizeMismatch {
            path: path.to_path_buf(),
            expected: artifact.size,
            actual: size,
        });
    }
    if digest != artifact.sha256 {
        return Err(ModelStoreError::ChecksumMismatch {
            path: path.to_path_buf(),
            expected: artifact.sha256,
            actual: digest.to_string(),
        });
    }
    Ok(())
}

struct DigestWriter<'a>(&'a mut Sha256);

impl Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ARTIFACTS: &[ModelArtifact] = &[ModelArtifact {
        kind: ModelArtifactKind::CharacterDictionary,
        filename: "hello.txt",
        url: "https://example.com/hello.txt",
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        size: 5,
    }];
    const TEST_MANIFEST: ModelManifest = ModelManifest {
        schema_version: 1,
        id: "test-model",
        revision: "v1",
        artifacts: TEST_ARTIFACTS,
    };

    #[derive(Debug)]
    struct StaticDownloader {
        bytes: &'static [u8],
        requests: std::sync::Mutex<Vec<ModelArtifactKind>>,
    }

    impl StaticDownloader {
        fn new(bytes: &'static [u8]) -> Self {
            Self {
                bytes,
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl ModelDownloader for StaticDownloader {
        type Error = io::Error;

        fn open(&self, artifact: &ModelArtifact) -> Result<Box<dyn Read + Send>, Self::Error> {
            self.requests.lock().unwrap().push(artifact.kind);
            Ok(Box::new(io::Cursor::new(self.bytes)))
        }
    }

    #[test]
    fn pinned_pp_ocr_manifest_is_well_formed() {
        validate_manifest(&PP_OCR_V6_SMALL).unwrap();
        assert_eq!(PP_OCR_V6_SMALL.artifacts.len(), 3);
    }

    #[test]
    fn installs_and_resolves_verified_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let installed = store
            .install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello"[..],
            )
            .unwrap();
        assert!(installed.ends_with("hello.txt"));

        let resolved = store.resolve(&TEST_MANIFEST).unwrap();
        assert_eq!(
            resolved.get(ModelArtifactKind::CharacterDictionary),
            Some(installed.as_path())
        );
    }

    #[test]
    fn rejected_install_does_not_poison_cache() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        assert!(matches!(
            store.install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"HELLO"[..],
            ),
            Err(ModelStoreError::ChecksumMismatch { .. })
        ));
        assert!(matches!(
            store.resolve(&TEST_MANIFEST),
            Err(ModelStoreError::MissingArtifact { .. })
        ));
    }

    #[test]
    fn oversized_install_stops_after_one_extra_byte() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        assert!(matches!(
            store.install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello and far too much data"[..],
            ),
            Err(ModelStoreError::SizeMismatch {
                expected: 5,
                actual: 6,
                ..
            })
        ));
    }

    #[test]
    fn install_replaces_invalid_cache_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let root = store.manifest_cache_root(&TEST_MANIFEST);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("hello.txt"), b"HELLO").unwrap();

        store
            .install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello"[..],
            )
            .unwrap();
        assert_eq!(fs::read(root.join("hello.txt")).unwrap(), b"hello");
    }

    #[test]
    fn stale_partial_installs_are_swept_under_the_lock() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let root = store.manifest_cache_root(&TEST_MANIFEST);
        fs::create_dir_all(&root).unwrap();
        let stale = root.join(".hello.txt.123.0.part");
        fs::write(&stale, b"stale").unwrap();

        store
            .install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello"[..],
            )
            .unwrap();
        assert!(!stale.exists());
    }

    #[test]
    fn manifest_paths_cannot_escape_the_cache() {
        const BAD_ID: ModelManifest = ModelManifest {
            id: "../escape",
            ..TEST_MANIFEST
        };
        const BAD_REVISION: ModelManifest = ModelManifest {
            revision: "nested/revision",
            ..TEST_MANIFEST
        };
        const BAD_FILENAME_ARTIFACTS: &[ModelArtifact] = &[ModelArtifact {
            filename: "hello.txt/",
            ..TEST_ARTIFACTS[0]
        }];
        const BAD_FILENAME: ModelManifest = ModelManifest {
            artifacts: BAD_FILENAME_ARTIFACTS,
            ..TEST_MANIFEST
        };

        for manifest in [&BAD_ID, &BAD_REVISION, &BAD_FILENAME] {
            assert!(matches!(
                validate_manifest(manifest),
                Err(ModelStoreError::InvalidManifest(_))
            ));
        }
    }

    #[test]
    fn explicit_override_is_verified_without_copying() {
        let cache = tempfile::tempdir().unwrap();
        let override_dir = tempfile::tempdir().unwrap();
        fs::write(override_dir.path().join("hello.txt"), b"hello").unwrap();
        let store = ModelStore::new(cache.path()).override_root(override_dir.path());
        let resolved = store.resolve(&TEST_MANIFEST).unwrap();
        assert_eq!(
            resolved.get(ModelArtifactKind::CharacterDictionary),
            Some(override_dir.path().join("hello.txt").as_path())
        );
    }

    #[test]
    fn concurrent_installs_converge_on_one_verified_file() {
        let temp = tempfile::tempdir().unwrap();
        let first_store = ModelStore::new(temp.path());
        let second_store = first_store.clone();
        let first = std::thread::spawn(move || {
            first_store.install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello"[..],
            )
        });
        let second = std::thread::spawn(move || {
            second_store.install(
                &TEST_MANIFEST,
                ModelArtifactKind::CharacterDictionary,
                &b"hello"[..],
            )
        });
        let first = first.join().unwrap().unwrap();
        let second = second.join().unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::read(first).unwrap(), b"hello");
    }

    #[test]
    fn resolve_or_download_fetches_once_then_reuses_verified_cache() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let downloader = StaticDownloader::new(b"hello");

        let first = store
            .resolve_or_download(&TEST_MANIFEST, ModelDownloadPolicy::IfMissing, &downloader)
            .unwrap();
        let second = store
            .resolve_or_download(&TEST_MANIFEST, ModelDownloadPolicy::IfMissing, &downloader)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(downloader.request_count(), 1);
    }

    #[test]
    fn concurrent_resolve_downloads_once_under_the_artifact_lock() {
        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let downloader = std::sync::Arc::new(StaticDownloader::new(b"hello"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let store = store.clone();
                let downloader = std::sync::Arc::clone(&downloader);
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.resolve_or_download(
                        &TEST_MANIFEST,
                        ModelDownloadPolicy::IfMissing,
                        downloader.as_ref(),
                    )
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(downloader.request_count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cache_io_failures_do_not_trigger_downloads() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let store = ModelStore::new(temp.path());
        let root = store.manifest_cache_root(&TEST_MANIFEST);
        fs::create_dir_all(&root).unwrap();
        symlink("hello.txt", root.join("hello.txt")).unwrap();
        let downloader = StaticDownloader::new(b"hello");

        assert!(matches!(
            store.resolve_or_download(&TEST_MANIFEST, ModelDownloadPolicy::IfMissing, &downloader,),
            Err(ModelAcquireError::Store(ModelStoreError::Io { .. }))
        ));
        assert_eq!(downloader.request_count(), 0);
    }

    #[test]
    fn resolve_or_download_honors_offline_and_explicit_directory_boundaries() {
        let cache = tempfile::tempdir().unwrap();
        let override_dir = tempfile::tempdir().unwrap();
        let downloader = StaticDownloader::new(b"hello");

        let offline = ModelStore::new(cache.path())
            .resolve_or_download(&TEST_MANIFEST, ModelDownloadPolicy::Offline, &downloader)
            .unwrap_err();
        assert!(matches!(
            offline,
            ModelAcquireError::DownloadsDisabled { .. }
        ));

        let explicit = ModelStore::new(cache.path())
            .override_root(override_dir.path())
            .resolve_or_download(&TEST_MANIFEST, ModelDownloadPolicy::IfMissing, &downloader)
            .unwrap_err();
        assert!(matches!(
            explicit,
            ModelAcquireError::ExplicitDirectoryIncomplete { .. }
        ));
        assert_eq!(downloader.request_count(), 0);
    }
}
