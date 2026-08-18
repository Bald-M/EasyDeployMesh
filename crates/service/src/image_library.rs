use easydeploymesh_core::{GhoImageCapability, ImageArtifact, ImageFormat};
use easydeploymesh_gho::{Compression as GhoCompression, PARSER_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(windows)]
use std::process::Command;
use std::{
    cmp::Reverse,
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::RwLock,
};
use thiserror::Error;
use uuid::Uuid;

const MANIFEST_SCHEMA_VERSION: u32 = 1;
const WIM_HEADER_PROBE_SIZE: usize = 48;
const WIM_MIN_HEADER_SIZE: u32 = 208;
const WIM_MAX_HEADER_SIZE: u32 = 4 * 1024;
const WIM_MAX_IMAGE_COUNT: u32 = 4 * 1024;
const WIM_SIGNATURE: &[u8; 8] = b"MSWIM\0\0\0";
const LEGACY_GHO_DEPLOYMENT_FILE_NAME: &str = "gho-deployment.json";
const LEGACY_GHOST_TOOL_FILE_NAME: &str = "Ghost64.exe";
const MAX_GHO_EXPANDED_BYTES: u64 = 16 * 1024 * 1024 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ImageLibraryError {
    #[error("image path does not point to a file: {0}")]
    NotAFile(String),
    #[error("unsupported image format: {0}")]
    UnsupportedFormat(String),
    #[error("invalid split-image set for {path}: {reason}")]
    InvalidSpanSet { path: String, reason: String },
    #[error("invalid WIM/ESD container at {path}: {reason}")]
    InvalidWimContainer { path: String, reason: String },
    #[error("image {id} is not present in the image library")]
    ImageNotFound { id: Uuid },
    #[error("image {id} uses catalog-only format {format:?}")]
    CatalogOnlyFormat { id: Uuid, format: ImageFormat },
    #[error("image {id} is not verified for deployment")]
    ImageNotVerified { id: Uuid },
    #[error("image {id} unexpectedly has span files and is not a standalone WIM/ESD")]
    DeployableImageHasSpans { id: Uuid },
    #[error("managed file for image {id} is unavailable at {path}: {source}")]
    ManagedFileUnavailable {
        id: Uuid,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("image {id} points outside the managed image store: {path}")]
    UnmanagedImagePath { id: Uuid, path: String },
    #[error("image {id} has no recorded SHA-256 digest")]
    MissingChecksum { id: Uuid },
    #[error("managed file size for image {id} changed: expected {expected} bytes, found {actual}")]
    ManagedSizeMismatch {
        id: Uuid,
        expected: u64,
        actual: u64,
    },
    #[error("managed file SHA-256 for image {id} changed: expected {expected}, found {actual}")]
    ManagedHashMismatch {
        id: Uuid,
        expected: String,
        actual: String,
    },
    #[error(
        "WIM/ESD image index {requested} is unavailable for image {id}; container has {image_count} images"
    )]
    ImageIndexOutOfRange {
        id: Uuid,
        requested: u32,
        image_count: u32,
    },
    #[error("DISM rejected WIM/ESD image at {path} for index {requested_index:?}: {reason}")]
    DismValidationFailed {
        path: String,
        requested_index: Option<u32>,
        reason: String,
    },
    #[error("image library lock was poisoned")]
    LockPoisoned,
    #[error("could not read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("could not copy image from {from} to {to}: {source}")]
    Copy {
        from: String,
        to: String,
        #[source]
        source: io::Error,
    },
    #[error("image library manifest is invalid: {0}")]
    InvalidManifest(#[from] serde_json::Error),
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImageManifest {
    schema_version: u32,
    images: Vec<ImageArtifact>,
}

#[derive(Debug)]
pub struct ImageLibrary {
    manifest_path: PathBuf,
    objects_dir: PathBuf,
    images: RwLock<Vec<ImageArtifact>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGhoImageFile {
    pub basename: String,
    pub canonical_path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGhoImageSet {
    pub artifact_id: Uuid,
    pub primary: PreparedGhoImageFile,
    pub spans: Vec<PreparedGhoImageFile>,
    pub total_size_bytes: u64,
    pub image_set_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGhoDeployment {
    pub image: PreparedGhoImageSet,
    pub capability: GhoImageCapability,
}

impl ImageLibrary {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, ImageLibraryError> {
        let data_dir = data_dir.as_ref();
        fs::create_dir_all(data_dir).map_err(|source| ImageLibraryError::Write {
            path: data_dir.display().to_string(),
            source,
        })?;
        let data_dir = fs::canonicalize(data_dir).map_err(|source| ImageLibraryError::Read {
            path: data_dir.display().to_string(),
            source,
        })?;
        let objects_path = data_dir.join("objects");
        fs::create_dir_all(&objects_path).map_err(|source| ImageLibraryError::Write {
            path: objects_path.display().to_string(),
            source,
        })?;
        let objects_dir =
            fs::canonicalize(&objects_path).map_err(|source| ImageLibraryError::Read {
                path: objects_path.display().to_string(),
                source,
            })?;

        let manifest_path = data_dir.join("images.json");
        let mut images = if manifest_path.exists() {
            let bytes = fs::read(&manifest_path).map_err(|source| ImageLibraryError::Read {
                path: manifest_path.display().to_string(),
                source,
            })?;
            let manifest: ImageManifest = serde_json::from_slice(&bytes)?;
            manifest.images
        } else {
            Vec::new()
        };
        let mut verification_changed = false;
        for image in &mut images {
            let deployable_format = matches!(image.format, ImageFormat::Wim | ImageFormat::Esd)
                || image.gho_capability.as_ref().is_some_and(|capability| {
                    capability.deployable && capability.parser_version == PARSER_VERSION
                });
            let managed_paths = artifact_paths_are_managed(image, &objects_dir);
            if image.verified && (!deployable_format || !managed_paths) {
                image.verified = false;
                verification_changed = true;
            }
            if image.format == ImageFormat::Gho
                && let Some(directory) = managed_object_directory(image, &objects_dir)
            {
                let _ = fs::remove_file(directory.join(LEGACY_GHO_DEPLOYMENT_FILE_NAME));
                let _ = fs::remove_file(directory.join(LEGACY_GHOST_TOOL_FILE_NAME));
            }
        }
        if verification_changed {
            persist_manifest(&manifest_path, &images)?;
        }

        Ok(Self {
            manifest_path,
            objects_dir,
            images: RwLock::new(images),
        })
    }

    pub fn list(&self) -> Result<Vec<ImageArtifact>, ImageLibraryError> {
        let mut images = self
            .images
            .read()
            .map_err(|_| ImageLibraryError::LockPoisoned)?
            .clone();
        images.sort_by_key(|image| Reverse(image.created_at));
        Ok(images)
    }

    pub fn contains(&self, id: Uuid) -> Result<bool, ImageLibraryError> {
        Ok(self
            .images
            .read()
            .map_err(|_| ImageLibraryError::LockPoisoned)?
            .iter()
            .any(|image| image.id == id))
    }

    pub fn get(&self, id: Uuid) -> Result<Option<ImageArtifact>, ImageLibraryError> {
        let image = self
            .images
            .read()
            .map_err(|_| ImageLibraryError::LockPoisoned)?
            .iter()
            .find(|image| image.id == id)
            .cloned();
        Ok(image)
    }

    pub fn import(&self, path: impl AsRef<Path>) -> Result<ImageArtifact, ImageLibraryError> {
        let canonical_path =
            fs::canonicalize(path.as_ref()).map_err(|source| ImageLibraryError::Read {
                path: path.as_ref().display().to_string(),
                source,
            })?;

        if !canonical_path.is_file() {
            return Err(ImageLibraryError::NotAFile(
                canonical_path.display().to_string(),
            ));
        }

        let format = detect_image_format(&canonical_path)?;
        if matches!(format, ImageFormat::Wim | ImageFormat::Esd) {
            validate_wim_header(&canonical_path)?;
        }
        let span_paths = discover_spans(&canonical_path, format)?;
        let staged = stage_managed_copy(&self.objects_dir, &canonical_path, &span_paths)?;
        let staged_paths = staged.all_paths();
        let gho_capability = (format == ImageFormat::Gho)
            .then(|| inspect_native_gho(&staged.primary_path(), !span_paths.is_empty()));
        if matches!(format, ImageFormat::Wim | ImageFormat::Esd) {
            validate_wim_header(&staged.primary_path())?;
            validate_wim_with_dism(&staged.primary_path(), None)?;
        }

        let staged_sizes = staged_paths
            .iter()
            .map(|image_path| {
                fs::metadata(image_path)
                    .map(|metadata| metadata.len())
                    .map_err(|source| ImageLibraryError::Read {
                        path: image_path.display().to_string(),
                        source,
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let size_bytes = staged_sizes
            .iter()
            .fold(0_u64, |total, size| total.saturating_add(*size));
        let sha256 = hash_files(&staged_paths)?;
        let name = canonical_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled image")
            .to_owned();

        let mut images = self
            .images
            .write()
            .map_err(|_| ImageLibraryError::LockPoisoned)?;

        let matching_image = images.iter().find_map(|image| {
            if image.format != format
                || image.size_bytes != size_bytes
                || image.sha256.as_deref() != Some(sha256.as_str())
                || !artifact_basenames_match(image, &staged_paths)
            {
                return None;
            }
            let managed_matches = managed_artifact_matches(image, &self.objects_dir);
            if managed_matches && !artifact_file_sizes_match(image, &staged_sizes) {
                return None;
            }
            Some((image, managed_matches))
        });
        if let Some((existing, true)) = matching_image {
            return Ok(existing.clone());
        }

        let artifact_id = matching_image.map_or_else(Uuid::new_v4, |(image, _)| image.id);
        let managed = staged.commit(&self.objects_dir)?;
        let created_at = chrono::Utc::now();
        let artifact = ImageArtifact {
            id: artifact_id,
            name,
            format,
            source_path: managed.primary.display().to_string(),
            size_bytes,
            sha256: Some(sha256),
            spans: managed
                .spans
                .iter()
                .map(|span| span.display().to_string())
                .collect(),
            verified: matches!(format, ImageFormat::Wim | ImageFormat::Esd)
                || gho_capability
                    .as_ref()
                    .is_some_and(|value| value.deployable),
            gho_capability,
            created_at,
        };

        let mut next_images = images.clone();
        next_images.retain(|image| image.id != artifact_id);
        next_images.push(artifact.clone());
        if let Err(error) = self.persist(&next_images) {
            let _ = fs::remove_dir_all(&managed.directory);
            return Err(error);
        }
        *images = next_images;

        Ok(artifact)
    }

    pub fn revalidate_for_deployment(
        &self,
        id: Uuid,
        requested_image_index: u32,
    ) -> Result<ImageArtifact, ImageLibraryError> {
        let artifact = self
            .images
            .read()
            .map_err(|_| ImageLibraryError::LockPoisoned)?
            .iter()
            .find(|image| image.id == id)
            .cloned()
            .ok_or(ImageLibraryError::ImageNotFound { id })?;

        if !matches!(artifact.format, ImageFormat::Wim | ImageFormat::Esd) {
            return Err(ImageLibraryError::CatalogOnlyFormat {
                id,
                format: artifact.format,
            });
        }
        if !artifact.verified {
            return Err(ImageLibraryError::ImageNotVerified { id });
        }
        if !artifact.spans.is_empty() {
            return Err(ImageLibraryError::DeployableImageHasSpans { id });
        }
        let expected_sha256 = artifact
            .sha256
            .as_deref()
            .ok_or(ImageLibraryError::MissingChecksum { id })?;
        let managed_path = resolve_managed_file(id, &artifact.source_path, &self.objects_dir)?;
        let actual_size = fs::metadata(&managed_path)
            .map_err(|source| ImageLibraryError::ManagedFileUnavailable {
                id,
                path: managed_path.display().to_string(),
                source,
            })?
            .len();
        if actual_size != artifact.size_bytes {
            return Err(ImageLibraryError::ManagedSizeMismatch {
                id,
                expected: artifact.size_bytes,
                actual: actual_size,
            });
        }
        let actual_sha256 = hash_files(std::slice::from_ref(&managed_path))?;
        if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
            return Err(ImageLibraryError::ManagedHashMismatch {
                id,
                expected: expected_sha256.to_owned(),
                actual: actual_sha256,
            });
        }

        let image_count = validate_wim_header(&managed_path)?;
        if !(1..=image_count).contains(&requested_image_index) {
            return Err(ImageLibraryError::ImageIndexOutOfRange {
                id,
                requested: requested_image_index,
                image_count,
            });
        }
        validate_wim_with_dism(&managed_path, Some(requested_image_index))?;

        Ok(artifact)
    }

    pub fn verify_gho_image(&self, id: Uuid) -> Result<ImageArtifact, ImageLibraryError> {
        let mut images = self
            .images
            .write()
            .map_err(|_| ImageLibraryError::LockPoisoned)?;
        let index = images
            .iter()
            .position(|image| image.id == id)
            .ok_or(ImageLibraryError::ImageNotFound { id })?;
        if images[index].format != ImageFormat::Gho {
            return Err(ImageLibraryError::UnsupportedFormat(
                "native GHO verification requires a GHO image".to_owned(),
            ));
        }
        let managed = resolve_managed_file(id, &images[index].source_path, &self.objects_dir)?;
        let capability = inspect_native_gho(&managed, !images[index].spans.is_empty());
        let mut updated = images[index].clone();
        updated.verified = capability.deployable;
        updated.gho_capability = Some(capability);
        let mut next = images.clone();
        next[index] = updated.clone();
        self.persist(&next)?;
        *images = next;
        Ok(updated)
    }

    pub fn prepare_gho_readiness(
        &self,
        id: Uuid,
    ) -> Result<PreparedGhoImageSet, ImageLibraryError> {
        let artifact = self
            .images
            .read()
            .map_err(|_| ImageLibraryError::LockPoisoned)?
            .iter()
            .find(|image| image.id == id)
            .cloned()
            .ok_or(ImageLibraryError::ImageNotFound { id })?;

        if artifact.format != ImageFormat::Gho {
            return Err(ImageLibraryError::UnsupportedFormat(format!(
                "GHO readiness requires GHO, found {:?} for image {id}",
                artifact.format
            )));
        }
        let expected_sha256 = artifact
            .sha256
            .as_deref()
            .ok_or(ImageLibraryError::MissingChecksum { id })?;
        let object_directory = resolve_gho_object_directory(id, &artifact, &self.objects_dir)?;
        let managed_files = read_directory_files(&object_directory)?;
        let discovered_spans =
            discover_ghost_spans(Path::new(&artifact.source_path), &managed_files)?;
        let recorded_spans = artifact.spans.iter().map(PathBuf::from).collect::<Vec<_>>();
        if discovered_spans != recorded_spans {
            return Err(ImageLibraryError::InvalidSpanSet {
                path: artifact.source_path.clone(),
                reason: "stored GHO span list no longer matches the managed object directory"
                    .to_owned(),
            });
        }
        let paths = artifact_file_paths(&artifact);
        let mut image_set_hasher = Sha256::new();
        let mut prepared_files = Vec::with_capacity(paths.len());
        let mut total_size_bytes = 0_u64;
        for (index, path) in paths.iter().enumerate() {
            prepared_files.push(prepare_gho_file(
                id,
                path,
                &object_directory,
                index == 0,
                &mut image_set_hasher,
                &mut total_size_bytes,
            )?);
        }
        let image_set_sha256 = format!("{:x}", image_set_hasher.finalize());
        if total_size_bytes != artifact.size_bytes {
            return Err(ImageLibraryError::ManagedSizeMismatch {
                id,
                expected: artifact.size_bytes,
                actual: total_size_bytes,
            });
        }
        if !image_set_sha256.eq_ignore_ascii_case(expected_sha256) {
            return Err(ImageLibraryError::ManagedHashMismatch {
                id,
                expected: expected_sha256.to_owned(),
                actual: image_set_sha256,
            });
        }

        let mut prepared_files = prepared_files.into_iter();
        let primary = prepared_files
            .next()
            .expect("an image artifact always has a primary path");
        Ok(PreparedGhoImageSet {
            artifact_id: id,
            primary,
            spans: prepared_files.collect(),
            total_size_bytes,
            image_set_sha256,
        })
    }

    pub fn prepare_gho_deployment(
        &self,
        id: Uuid,
    ) -> Result<PreparedGhoDeployment, ImageLibraryError> {
        let image = self.prepare_gho_readiness(id)?;
        if !image.spans.is_empty() {
            return Err(ImageLibraryError::InvalidSpanSet {
                path: image.primary.canonical_path.display().to_string(),
                reason: "spanned GHO deployment is not supported".to_owned(),
            });
        }
        let artifact = self
            .get(id)?
            .ok_or(ImageLibraryError::ImageNotFound { id })?;
        let expected = artifact
            .gho_capability
            .filter(|value| value.deployable)
            .ok_or(ImageLibraryError::ImageNotVerified { id })?;
        let actual = inspect_native_gho(&image.primary.canonical_path, false);
        if !actual.deployable
            || actual.expanded_size_bytes != expected.expanded_size_bytes
            || actual.expanded_sha256 != expected.expanded_sha256
        {
            return Err(ImageLibraryError::ManagedHashMismatch {
                id,
                expected: expected.expanded_sha256.unwrap_or_default(),
                actual: actual
                    .expanded_sha256
                    .unwrap_or_else(|| actual.blocked_reason.unwrap_or_default()),
            });
        }
        Ok(PreparedGhoDeployment {
            image,
            capability: actual,
        })
    }

    pub fn remove(&self, id: Uuid) -> Result<bool, ImageLibraryError> {
        let mut images = self
            .images
            .write()
            .map_err(|_| ImageLibraryError::LockPoisoned)?;
        let removed_image = images.iter().find(|image| image.id == id).cloned();
        let mut next_images = images.clone();
        next_images.retain(|image| image.id != id);

        let Some(removed_image) = removed_image else {
            return Ok(false);
        };
        let cleanup_directory =
            managed_object_directory(&removed_image, &self.objects_dir).filter(|directory| {
                !next_images.iter().any(|remaining| {
                    managed_object_directory(remaining, &self.objects_dir).as_ref()
                        == Some(directory)
                })
            });

        self.persist(&next_images)?;
        *images = next_images;
        drop(images);
        if let Some(directory) = cleanup_directory {
            fs::remove_dir_all(&directory).map_err(|source| ImageLibraryError::Write {
                path: directory.display().to_string(),
                source,
            })?;
        }
        Ok(true)
    }

    fn persist(&self, images: &[ImageArtifact]) -> Result<(), ImageLibraryError> {
        persist_manifest(&self.manifest_path, images)
    }
}

fn persist_manifest(
    manifest_path: &Path,
    images: &[ImageArtifact],
) -> Result<(), ImageLibraryError> {
    let manifest = ImageManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        images: images.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(manifest_path, bytes).map_err(|source| ImageLibraryError::Write {
        path: manifest_path.display().to_string(),
        source,
    })
}

#[derive(Debug)]
struct StagedImage {
    directory: PathBuf,
    primary_name: OsString,
    span_names: Vec<OsString>,
    committed: bool,
}

impl StagedImage {
    fn primary_path(&self) -> PathBuf {
        self.directory.join(&self.primary_name)
    }

    fn all_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(self.span_names.len() + 1);
        paths.push(self.primary_path());
        paths.extend(self.span_names.iter().map(|name| self.directory.join(name)));
        paths
    }

    fn commit(mut self, objects_dir: &Path) -> Result<ManagedImage, ImageLibraryError> {
        let final_directory = objects_dir.join(Uuid::new_v4().to_string());
        fs::rename(&self.directory, &final_directory).map_err(|source| {
            ImageLibraryError::Write {
                path: final_directory.display().to_string(),
                source,
            }
        })?;
        self.committed = true;
        Ok(ManagedImage {
            primary: final_directory.join(&self.primary_name),
            spans: self
                .span_names
                .iter()
                .map(|name| final_directory.join(name))
                .collect(),
            directory: final_directory,
        })
    }
}

impl Drop for StagedImage {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

#[derive(Debug)]
struct ManagedImage {
    directory: PathBuf,
    primary: PathBuf,
    spans: Vec<PathBuf>,
}

fn stage_managed_copy(
    objects_dir: &Path,
    primary_path: &Path,
    span_paths: &[PathBuf],
) -> Result<StagedImage, ImageLibraryError> {
    let primary_name = source_basename(primary_path)?;
    let span_names = span_paths
        .iter()
        .map(|path| source_basename(path))
        .collect::<Result<Vec<_>, _>>()?;
    reject_windows_name_collisions(primary_path, &primary_name, span_paths, &span_names)?;
    let directory = objects_dir.join(format!(".import-{}.tmp", Uuid::new_v4()));
    fs::create_dir(&directory).map_err(|source| ImageLibraryError::Write {
        path: directory.display().to_string(),
        source,
    })?;
    let staged = StagedImage {
        directory,
        primary_name,
        span_names,
        committed: false,
    };

    copy_and_sync(primary_path, &staged.primary_path())?;
    for (source_path, destination_name) in span_paths.iter().zip(&staged.span_names) {
        copy_and_sync(source_path, &staged.directory.join(destination_name))?;
    }

    Ok(staged)
}

fn source_basename(path: &Path) -> Result<OsString, ImageLibraryError> {
    path.file_name()
        .map(OsString::from)
        .ok_or_else(|| ImageLibraryError::InvalidSpanSet {
            path: path.display().to_string(),
            reason: "a managed image file has no basename".to_owned(),
        })
}

fn reject_windows_name_collisions(
    primary_path: &Path,
    primary_name: &OsString,
    span_paths: &[PathBuf],
    span_names: &[OsString],
) -> Result<(), ImageLibraryError> {
    let mut names = HashSet::with_capacity(span_names.len() + 1);
    names.insert(windows_name_key(primary_name));
    for (span_path, span_name) in span_paths.iter().zip(span_names) {
        if !names.insert(windows_name_key(span_name)) {
            return Err(ImageLibraryError::InvalidSpanSet {
                path: primary_path.display().to_string(),
                reason: format!(
                    "{} collides with another image filename when compared case-insensitively",
                    span_path.display()
                ),
            });
        }
    }
    Ok(())
}

fn windows_name_key(name: &OsStr) -> String {
    name.to_string_lossy().to_uppercase()
}

fn copy_and_sync(source_path: &Path, destination_path: &Path) -> Result<(), ImageLibraryError> {
    fs::copy(source_path, destination_path).map_err(|source| ImageLibraryError::Copy {
        from: source_path.display().to_string(),
        to: destination_path.display().to_string(),
        source,
    })?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination_path)
        .and_then(|file| file.sync_all())
        .map_err(|source| ImageLibraryError::Write {
            path: destination_path.display().to_string(),
            source,
        })
}

fn managed_artifact_matches(image: &ImageArtifact, objects_dir: &Path) -> bool {
    let paths = artifact_file_paths(image);
    if !artifact_paths_are_managed(image, objects_dir) {
        return false;
    }
    let Ok(size_bytes) = paths.iter().try_fold(0_u64, |total, path| {
        fs::metadata(path).map(|metadata| total.saturating_add(metadata.len()))
    }) else {
        return false;
    };
    if size_bytes != image.size_bytes {
        return false;
    }
    image.sha256.as_deref().is_some_and(|expected| {
        hash_files(&paths).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))
    })
}

fn artifact_basenames_match(image: &ImageArtifact, staged_paths: &[PathBuf]) -> bool {
    let managed_paths = artifact_file_paths(image);
    managed_paths.len() == staged_paths.len()
        && managed_paths
            .iter()
            .zip(staged_paths)
            .all(|(managed, staged)| {
                managed.file_name().is_some() && managed.file_name() == staged.file_name()
            })
}

fn artifact_file_sizes_match(image: &ImageArtifact, staged_sizes: &[u64]) -> bool {
    let managed_paths = artifact_file_paths(image);
    managed_paths.len() == staged_sizes.len()
        && managed_paths
            .iter()
            .zip(staged_sizes)
            .all(|(managed, staged_size)| {
                fs::metadata(managed).is_ok_and(|metadata| metadata.len() == *staged_size)
            })
}

fn artifact_file_paths(image: &ImageArtifact) -> Vec<PathBuf> {
    let mut paths = Vec::with_capacity(image.spans.len() + 1);
    paths.push(PathBuf::from(&image.source_path));
    paths.extend(image.spans.iter().map(PathBuf::from));
    paths
}

fn artifact_paths_are_managed(image: &ImageArtifact, objects_dir: &Path) -> bool {
    std::iter::once(image.source_path.as_str())
        .chain(image.spans.iter().map(String::as_str))
        .all(|path| canonical_path_is_managed(Path::new(path), objects_dir))
}

fn managed_object_directory(image: &ImageArtifact, objects_dir: &Path) -> Option<PathBuf> {
    let source_parent = Path::new(&image.source_path).parent()?;
    if source_parent.parent()? != objects_dir {
        return None;
    }
    Uuid::parse_str(source_parent.file_name()?.to_str()?).ok()?;
    let metadata = fs::symlink_metadata(source_parent).ok()?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return None;
    }
    let canonical_directory = fs::canonicalize(source_parent).ok()?;
    if canonical_directory.parent()? != objects_dir {
        return None;
    }
    if image
        .spans
        .iter()
        .any(|span| Path::new(span).parent() != Some(source_parent))
    {
        return None;
    }
    Some(canonical_directory)
}

fn canonical_path_is_managed(path: &Path, objects_dir: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|canonical_path| canonical_path.starts_with(objects_dir))
}

fn resolve_managed_file(
    id: Uuid,
    source_path: &str,
    objects_dir: &Path,
) -> Result<PathBuf, ImageLibraryError> {
    let path = Path::new(source_path);
    let canonical_path =
        fs::canonicalize(path).map_err(|source| ImageLibraryError::ManagedFileUnavailable {
            id,
            path: path.display().to_string(),
            source,
        })?;
    if !canonical_path.starts_with(objects_dir) {
        return Err(ImageLibraryError::UnmanagedImagePath {
            id,
            path: canonical_path.display().to_string(),
        });
    }
    if !canonical_path.is_file() {
        return Err(ImageLibraryError::ManagedFileUnavailable {
            id,
            path: canonical_path.display().to_string(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "managed path is not a file"),
        });
    }
    Ok(canonical_path)
}

fn resolve_gho_object_directory(
    id: Uuid,
    artifact: &ImageArtifact,
    objects_dir: &Path,
) -> Result<PathBuf, ImageLibraryError> {
    let primary_path = Path::new(&artifact.source_path);
    let object_directory =
        primary_path
            .parent()
            .ok_or_else(|| ImageLibraryError::UnmanagedImagePath {
                id,
                path: primary_path.display().to_string(),
            })?;
    let is_direct_object = object_directory.parent() == Some(objects_dir)
        && object_directory
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| Uuid::parse_str(name).is_ok());
    if !is_direct_object
        || artifact
            .spans
            .iter()
            .any(|span| Path::new(span).parent() != Some(object_directory))
    {
        return Err(ImageLibraryError::UnmanagedImagePath {
            id,
            path: primary_path.display().to_string(),
        });
    }

    let metadata = fs::symlink_metadata(object_directory).map_err(|source| {
        ImageLibraryError::ManagedFileUnavailable {
            id,
            path: object_directory.display().to_string(),
            source,
        }
    })?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ImageLibraryError::UnmanagedImagePath {
            id,
            path: object_directory.display().to_string(),
        });
    }
    let canonical_directory = fs::canonicalize(object_directory).map_err(|source| {
        ImageLibraryError::ManagedFileUnavailable {
            id,
            path: object_directory.display().to_string(),
            source,
        }
    })?;
    if canonical_directory.parent() != Some(objects_dir) {
        return Err(ImageLibraryError::UnmanagedImagePath {
            id,
            path: canonical_directory.display().to_string(),
        });
    }
    Ok(canonical_directory)
}

fn prepare_gho_file(
    id: Uuid,
    path: &Path,
    object_directory: &Path,
    is_primary: bool,
    image_set_hasher: &mut Sha256,
    total_size_bytes: &mut u64,
) -> Result<PreparedGhoImageFile, ImageLibraryError> {
    let basename = path
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty() && Path::new(name).components().count() == 1)
        .ok_or_else(|| ImageLibraryError::InvalidSpanSet {
            path: path.display().to_string(),
            reason: format!("{} has no safe Unicode basename", path.display()),
        })?
        .to_owned();
    let extension = path.extension().and_then(OsStr::to_str).unwrap_or_default();
    let extension_matches = if is_primary {
        extension.eq_ignore_ascii_case("gho")
    } else {
        extension.eq_ignore_ascii_case("ghs")
            || (extension.len() == 3 && extension.bytes().all(|byte| byte.is_ascii_digit()))
    };
    if !extension_matches {
        return Err(ImageLibraryError::InvalidSpanSet {
            path: path.display().to_string(),
            reason: format!(
                "{} does not have the expected {} filename",
                path.display(),
                if is_primary { "GHO" } else { "GHS/CNS span" }
            ),
        });
    }

    let metadata =
        fs::symlink_metadata(path).map_err(|source| ImageLibraryError::ManagedFileUnavailable {
            id,
            path: path.display().to_string(),
            source,
        })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ImageLibraryError::ManagedFileUnavailable {
            id,
            path: path.display().to_string(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "managed GHO path is not a regular file",
            ),
        });
    }
    let canonical_path =
        fs::canonicalize(path).map_err(|source| ImageLibraryError::ManagedFileUnavailable {
            id,
            path: path.display().to_string(),
            source,
        })?;
    if canonical_path.parent() != Some(object_directory) {
        return Err(ImageLibraryError::UnmanagedImagePath {
            id,
            path: canonical_path.display().to_string(),
        });
    }

    let file = File::open(&canonical_path).map_err(|source| {
        ImageLibraryError::ManagedFileUnavailable {
            id,
            path: canonical_path.display().to_string(),
            source,
        }
    })?;
    let mut reader = BufReader::new(file);
    let mut file_hasher = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .map_err(|source| ImageLibraryError::Read {
                path: canonical_path.display().to_string(),
                source,
            })?;
        if bytes_read == 0 {
            break;
        }
        let bytes = &buffer[..bytes_read];
        file_hasher.update(bytes);
        image_set_hasher.update(bytes);
        let bytes_read = u64::try_from(bytes_read).expect("buffer length should fit in u64");
        size_bytes = size_bytes.checked_add(bytes_read).ok_or_else(|| {
            ImageLibraryError::InvalidSpanSet {
                path: canonical_path.display().to_string(),
                reason: format!(
                    "{} is too large to measure safely",
                    canonical_path.display()
                ),
            }
        })?;
        *total_size_bytes = total_size_bytes.checked_add(bytes_read).ok_or_else(|| {
            ImageLibraryError::InvalidSpanSet {
                path: canonical_path.display().to_string(),
                reason: "managed GHO image set is too large to measure safely".to_owned(),
            }
        })?;
    }

    Ok(PreparedGhoImageFile {
        basename,
        canonical_path,
        size_bytes,
        sha256: format!("{:x}", file_hasher.finalize()),
    })
}

#[cfg(not(windows))]
fn validate_wim_with_dism(
    _path: &Path,
    _requested_index: Option<u32>,
) -> Result<(), ImageLibraryError> {
    Ok(())
}

#[cfg(windows)]
fn validate_wim_with_dism(
    path: &Path,
    requested_index: Option<u32>,
) -> Result<(), ImageLibraryError> {
    let mut wim_file_argument = OsString::from("/WimFile:");
    wim_file_argument.push(path.as_os_str());
    let mut arguments = vec![
        OsString::from("/English"),
        OsString::from("/Get-WimInfo"),
        wim_file_argument,
    ];
    if let Some(index) = requested_index {
        arguments.push(OsString::from(format!("/Index:{index}")));
    }
    let output = Command::new("dism.exe")
        .args(&arguments)
        .output()
        .map_err(|source| ImageLibraryError::DismValidationFailed {
            path: path.display().to_string(),
            requested_index,
            reason: format!("could not start dism.exe: {source}"),
        })?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        stdout.trim()
    } else {
        detail
    };
    let detail = if detail.is_empty() {
        format!("dism.exe exited with {}", output.status)
    } else {
        format!("dism.exe exited with {}: {detail}", output.status)
    };
    Err(ImageLibraryError::DismValidationFailed {
        path: path.display().to_string(),
        requested_index,
        reason: detail,
    })
}

fn validate_wim_header(path: &Path) -> Result<u32, ImageLibraryError> {
    let mut file = File::open(path).map_err(|source| ImageLibraryError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let file_size = file
        .metadata()
        .map_err(|source| ImageLibraryError::Read {
            path: path.display().to_string(),
            source,
        })?
        .len();
    let mut header = [0_u8; WIM_HEADER_PROBE_SIZE];
    if let Err(source) = file.read_exact(&mut header) {
        if source.kind() == io::ErrorKind::UnexpectedEof {
            return Err(invalid_wim_container(
                path,
                format!(
                    "header is truncated (need at least {WIM_HEADER_PROBE_SIZE} bytes, found {file_size})"
                ),
            ));
        }
        return Err(ImageLibraryError::Read {
            path: path.display().to_string(),
            source,
        });
    }

    if &header[..WIM_SIGNATURE.len()] != WIM_SIGNATURE {
        return Err(invalid_wim_container(path, "missing MSWIM signature"));
    }

    let header_size = u32::from_le_bytes(header[8..12].try_into().expect("fixed header slice"));
    if !(WIM_MIN_HEADER_SIZE..=WIM_MAX_HEADER_SIZE).contains(&header_size) {
        return Err(invalid_wim_container(
            path,
            format!(
                "header size {header_size} is outside {WIM_MIN_HEADER_SIZE}..={WIM_MAX_HEADER_SIZE}"
            ),
        ));
    }
    if u64::from(header_size) > file_size {
        return Err(invalid_wim_container(
            path,
            format!("declared header size {header_size} exceeds file size {file_size}"),
        ));
    }

    let image_count = u32::from_le_bytes(header[44..48].try_into().expect("fixed header slice"));
    if !(1..=WIM_MAX_IMAGE_COUNT).contains(&image_count) {
        return Err(invalid_wim_container(
            path,
            format!("image count {image_count} is outside 1..={WIM_MAX_IMAGE_COUNT}"),
        ));
    }

    Ok(image_count)
}

fn invalid_wim_container(path: &Path, reason: impl Into<String>) -> ImageLibraryError {
    ImageLibraryError::InvalidWimContainer {
        path: path.display().to_string(),
        reason: reason.into(),
    }
}

fn detect_image_format(path: &Path) -> Result<ImageFormat, ImageLibraryError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "gho" => Ok(ImageFormat::Gho),
        "wim" => Ok(ImageFormat::Wim),
        "esd" => Ok(ImageFormat::Esd),
        "swm" => Ok(ImageFormat::Swm),
        _ => Err(ImageLibraryError::UnsupportedFormat(extension)),
    }
}

fn discover_spans(
    primary_path: &Path,
    format: ImageFormat,
) -> Result<Vec<PathBuf>, ImageLibraryError> {
    if !matches!(format, ImageFormat::Gho | ImageFormat::Swm) {
        return Ok(Vec::new());
    }

    let directory = primary_path.parent().unwrap_or_else(|| Path::new("."));
    let files = read_directory_files(directory)?;
    match format {
        ImageFormat::Gho => discover_ghost_spans(primary_path, &files),
        ImageFormat::Swm => Ok(discover_swm_spans(primary_path, &files)),
        _ => unreachable!(),
    }
}

fn read_directory_files(directory: &Path) -> Result<Vec<PathBuf>, ImageLibraryError> {
    let entries = fs::read_dir(directory).map_err(|source| ImageLibraryError::Read {
        path: directory.display().to_string(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ImageLibraryError::Read {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::metadata(&path).map_err(|source| ImageLibraryError::Read {
            path: path.display().to_string(),
            source,
        })?;
        if metadata.is_file() {
            files.push(path);
        }
    }
    files.sort_by(|left, right| {
        let left_name = left.file_name().unwrap_or_else(|| left.as_os_str());
        let right_name = right.file_name().unwrap_or_else(|| right.as_os_str());
        windows_name_key(left_name)
            .cmp(&windows_name_key(right_name))
            .then_with(|| left.cmp(right))
    });
    Ok(files)
}

fn discover_swm_spans(primary_path: &Path, files: &[PathBuf]) -> Vec<PathBuf> {
    let primary_stem = primary_path
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_lowercase();
    files
        .iter()
        .filter(|candidate| candidate.as_path() != primary_path)
        .filter(|candidate| {
            candidate
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("swm"))
        })
        .filter(|candidate| {
            candidate
                .file_stem()
                .and_then(OsStr::to_str)
                .is_some_and(|stem| stem.to_lowercase().starts_with(&primary_stem))
        })
        .cloned()
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GhostSpanScheme {
    GhsThreeDigit,
    GhsFiveDigit,
    CnsNumericExtension,
}

impl GhostSpanScheme {
    fn description(self) -> &'static str {
        match self {
            Self::GhsThreeDigit => "three-digit GHS",
            Self::GhsFiveDigit => "five-digit GHS",
            Self::CnsNumericExtension => "CNS numeric-extension",
        }
    }
}

#[derive(Debug)]
struct GhostSpanCandidate {
    path: PathBuf,
    sequence: u32,
    scheme: GhostSpanScheme,
}

fn discover_ghost_spans(
    primary_path: &Path,
    files: &[PathBuf],
) -> Result<Vec<PathBuf>, ImageLibraryError> {
    let primary_stem = primary_path
        .file_stem()
        .and_then(OsStr::to_str)
        .ok_or_else(|| ImageLibraryError::InvalidSpanSet {
            path: primary_path.display().to_string(),
            reason: "the primary GHO basename is not valid Unicode".to_owned(),
        })?;
    let ghost_prefix = primary_stem.chars().take(5).collect::<String>();
    let mut candidates = Vec::new();

    for candidate in files {
        if candidate == primary_path {
            continue;
        }
        let Some(extension) = candidate.extension().and_then(OsStr::to_str) else {
            continue;
        };
        let Some(stem) = candidate.file_stem().and_then(OsStr::to_str) else {
            continue;
        };

        if extension.eq_ignore_ascii_case("ghs") {
            let Some(digits) = strip_name_prefix_for_windows(stem, &ghost_prefix) else {
                continue;
            };
            if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let scheme = match digits.len() {
                3 => GhostSpanScheme::GhsThreeDigit,
                5 => GhostSpanScheme::GhsFiveDigit,
                width => {
                    return Err(invalid_span_set(
                        primary_path,
                        format!(
                            "{} uses an unsupported {width}-digit GHS sequence",
                            candidate.display()
                        ),
                    ));
                }
            };
            candidates.push(GhostSpanCandidate {
                path: candidate.clone(),
                sequence: parse_span_sequence(primary_path, candidate, digits)?,
                scheme,
            });
        } else if extension.len() == 3
            && extension.bytes().all(|byte| byte.is_ascii_digit())
            && names_equal_for_windows(stem, primary_stem)
        {
            candidates.push(GhostSpanCandidate {
                path: candidate.clone(),
                sequence: parse_span_sequence(primary_path, candidate, extension)?,
                scheme: GhostSpanScheme::CnsNumericExtension,
            });
        }
    }

    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let scheme = candidates[0].scheme;
    if let Some(conflicting) = candidates
        .iter()
        .find(|candidate| candidate.scheme != scheme)
    {
        return Err(invalid_span_set(
            primary_path,
            format!(
                "{} mixes {} spans with {} spans",
                conflicting.path.display(),
                scheme.description(),
                conflicting.scheme.description()
            ),
        ));
    }

    if matches!(scheme, GhostSpanScheme::CnsNumericExtension) {
        reject_case_conflicting_cns_primary(primary_path, files, primary_stem)?;
    } else {
        reject_ambiguous_ghost_prefix(primary_path, files, &ghost_prefix)?;
    }

    candidates.sort_by(|left, right| {
        left.sequence.cmp(&right.sequence).then_with(|| {
            let left_name = left
                .path
                .file_name()
                .unwrap_or_else(|| left.path.as_os_str());
            let right_name = right
                .path
                .file_name()
                .unwrap_or_else(|| right.path.as_os_str());
            windows_name_key(left_name)
                .cmp(&windows_name_key(right_name))
                .then_with(|| left.path.cmp(&right.path))
        })
    });
    for (index, candidate) in candidates.iter().enumerate() {
        let expected = u32::try_from(index + 1).expect("span count should fit in u32");
        if candidate.sequence != expected {
            let reason = if index > 0 && candidate.sequence == candidates[index - 1].sequence {
                format!(
                    "{} duplicates span sequence {}",
                    candidate.path.display(),
                    candidate.sequence
                )
            } else {
                format!(
                    "{} has span sequence {}; expected {expected}",
                    candidate.path.display(),
                    candidate.sequence
                )
            };
            return Err(invalid_span_set(primary_path, reason));
        }
    }

    Ok(candidates
        .into_iter()
        .map(|candidate| candidate.path)
        .collect())
}

fn strip_name_prefix_for_windows<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let prefix_character_count = prefix.chars().count();
    let split_at = value
        .char_indices()
        .nth(prefix_character_count)
        .map_or(value.len(), |(index, _)| index);
    let (candidate_prefix, remainder) = value.split_at(split_at);
    names_equal_for_windows(candidate_prefix, prefix).then_some(remainder)
}

fn parse_span_sequence(
    primary_path: &Path,
    candidate: &Path,
    digits: &str,
) -> Result<u32, ImageLibraryError> {
    digits.parse().map_err(|_| {
        invalid_span_set(
            primary_path,
            format!("{} has an invalid span sequence", candidate.display()),
        )
    })
}

fn reject_ambiguous_ghost_prefix(
    primary_path: &Path,
    files: &[PathBuf],
    ghost_prefix: &str,
) -> Result<(), ImageLibraryError> {
    for candidate in files {
        if candidate == primary_path
            || !candidate
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gho"))
        {
            continue;
        }
        let Some(candidate_stem) = candidate.file_stem().and_then(OsStr::to_str) else {
            continue;
        };
        let candidate_prefix = candidate_stem.chars().take(5).collect::<String>();
        if names_equal_for_windows(&candidate_prefix, ghost_prefix) {
            return Err(invalid_span_set(
                primary_path,
                format!(
                    "{} shares the Ghost span prefix {ghost_prefix:?}",
                    candidate.display()
                ),
            ));
        }
    }
    Ok(())
}

fn reject_case_conflicting_cns_primary(
    primary_path: &Path,
    files: &[PathBuf],
    primary_stem: &str,
) -> Result<(), ImageLibraryError> {
    for candidate in files {
        if candidate == primary_path
            || !candidate
                .extension()
                .and_then(OsStr::to_str)
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gho"))
        {
            continue;
        }
        if candidate
            .file_stem()
            .and_then(OsStr::to_str)
            .is_some_and(|candidate_stem| names_equal_for_windows(candidate_stem, primary_stem))
        {
            return Err(invalid_span_set(
                primary_path,
                format!(
                    "{} conflicts with the CNS primary basename when compared case-insensitively",
                    candidate.display()
                ),
            ));
        }
    }
    Ok(())
}

fn names_equal_for_windows(left: &str, right: &str) -> bool {
    left.to_uppercase() == right.to_uppercase()
}

fn invalid_span_set(primary_path: &Path, reason: impl Into<String>) -> ImageLibraryError {
    ImageLibraryError::InvalidSpanSet {
        path: primary_path.display().to_string(),
        reason: reason.into(),
    }
}

fn hash_files(paths: &[PathBuf]) -> Result<String, ImageLibraryError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];

    for path in paths {
        let file = File::open(path).map_err(|source| ImageLibraryError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let mut reader = BufReader::new(file);
        loop {
            let bytes_read =
                reader
                    .read(&mut buffer)
                    .map_err(|source| ImageLibraryError::Read {
                        path: path.display().to_string(),
                        source,
                    })?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }
    }

    Ok(format!("{:x}", hasher.finalize()))
}

struct GhoAnalysisSink {
    digest: Sha256,
    prefix: Vec<u8>,
}

impl Write for GhoAnalysisSink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.prefix.len() < 512 {
            let wanted = (512 - self.prefix.len()).min(bytes.len());
            self.prefix.extend_from_slice(&bytes[..wanted]);
        }
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn inspect_native_gho(path: &Path, has_spans: bool) -> GhoImageCapability {
    let blocked = |reason: String| GhoImageCapability {
        deployable: false,
        compression: None,
        expanded_size_bytes: None,
        expanded_sha256: None,
        partition_count: None,
        source_partition: None,
        parser_version: PARSER_VERSION,
        blocked_reason: Some(reason),
    };
    if has_spans {
        return blocked("spanned_image_unsupported".to_owned());
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return blocked("image_unavailable".to_owned()),
    };
    let info = match easydeploymesh_gho::inspect(&mut file) {
        Ok(info) => info,
        Err(error) => return blocked(native_gho_error_code(&error).to_owned()),
    };
    let mut sink = GhoAnalysisSink {
        digest: Sha256::new(),
        prefix: Vec::new(),
    };
    let (_, expanded_size_bytes) = match easydeploymesh_gho::decode_partition(
        &mut file,
        info.source_partition,
        &mut sink,
        MAX_GHO_EXPANDED_BYTES,
    ) {
        Ok(result) => result,
        Err(error) => return blocked(native_gho_error_code(&error).to_owned()),
    };
    if sink.prefix.get(3..11) != Some(b"NTFS    ") {
        return blocked("non_ntfs_partition".to_owned());
    }
    GhoImageCapability {
        deployable: true,
        compression: Some(match info.compression {
            GhoCompression::None => "z0".to_owned(),
            GhoCompression::Fast => "z1".to_owned(),
            GhoCompression::High(level) => format!("z{level}"),
        }),
        expanded_size_bytes: Some(expanded_size_bytes),
        expanded_sha256: Some(format!("{:x}", sink.digest.finalize())),
        partition_count: Some(info.partition_count),
        source_partition: Some(info.source_partition),
        parser_version: PARSER_VERSION,
        blocked_reason: None,
    }
}

fn native_gho_error_code(error: &easydeploymesh_gho::Error) -> &'static str {
    match error {
        easydeploymesh_gho::Error::SpannedUnsupported => "spanned_image_unsupported",
        easydeploymesh_gho::Error::EncryptedUnsupported => "encrypted_image_unsupported",
        easydeploymesh_gho::Error::UnsupportedCompression(_) => "compression_unsupported",
        easydeploymesh_gho::Error::PartitionCount(_) => "partition_scope_unsupported",
        easydeploymesh_gho::Error::ExpandedLimit => "expanded_size_limit",
        easydeploymesh_gho::Error::Io(_) => "image_unavailable",
        _ => "invalid_or_corrupt_gho",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn create_image(path: &Path, contents: &[u8]) {
        let mut file = File::create(path).expect("fixture should be writable");
        file.write_all(contents)
            .expect("fixture contents should be writable");
    }

    fn create_wim(path: &Path, image_count: u32, payload: &[u8]) -> Vec<u8> {
        let mut contents = vec![0_u8; WIM_MIN_HEADER_SIZE as usize];
        contents[..8].copy_from_slice(WIM_SIGNATURE);
        contents[8..12].copy_from_slice(&WIM_MIN_HEADER_SIZE.to_le_bytes());
        contents[12..16].copy_from_slice(&0x0001_0d00_u32.to_le_bytes());
        contents[40..42].copy_from_slice(&1_u16.to_le_bytes());
        contents[42..44].copy_from_slice(&1_u16.to_le_bytes());
        contents[44..48].copy_from_slice(&image_count.to_le_bytes());
        contents.extend_from_slice(payload);
        create_image(path, &contents);
        contents
    }

    #[test]
    fn rejects_arbitrary_bytes_renamed_as_wim() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("not-really-windows.wim");
        create_image(
            &source_path,
            b"arbitrary bytes with a trusted-looking extension",
        );

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let error = library
            .import(&source_path)
            .expect_err("a file extension alone must not make an image deployable");

        assert!(matches!(
            error,
            ImageLibraryError::InvalidWimContainer { .. }
        ));
        assert!(library.list().expect("images should list").is_empty());
    }

    #[test]
    fn import_owns_a_managed_copy_after_the_original_is_deleted() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows 11.wim");
        let contents = create_wim(&source_path, 1, b"easydeploymesh-image");
        let original_path = fs::canonicalize(&source_path).expect("source should canonicalize");
        let library_dir = temp.path().join("library");

        let library = ImageLibrary::open(&library_dir).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let managed_path = PathBuf::from(&artifact.source_path);
        let managed_root = fs::canonicalize(&library_dir)
            .expect("library directory should canonicalize")
            .join("objects");

        assert_ne!(managed_path, original_path);
        assert!(managed_path.starts_with(managed_root));
        fs::remove_file(&source_path).expect("test should remove only its source fixture");
        assert_eq!(
            fs::read(&managed_path).expect("managed artifact should remain readable"),
            contents
        );
        assert!(artifact.verified);
    }

    #[test]
    fn deployment_revalidation_rejects_same_size_managed_copy_tampering() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("windows.wim");
        create_wim(&source_path, 1, b"original payload");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let managed_path = PathBuf::from(&artifact.source_path);
        let mut tampered = fs::read(&managed_path).expect("managed image should be readable");
        let last_byte = tampered.last_mut().expect("fixture should not be empty");
        *last_byte ^= 0xff;
        create_image(&managed_path, &tampered);

        let error = library
            .revalidate_for_deployment(artifact.id, 1)
            .expect_err("same-size tampering must fail preflight");

        assert!(matches!(
            error,
            ImageLibraryError::ManagedHashMismatch { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn deployment_revalidation_rejects_changed_managed_copy_size() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("windows.esd");
        create_wim(&source_path, 1, b"original payload");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let managed_path = PathBuf::from(&artifact.source_path);
        let mut managed = fs::OpenOptions::new()
            .append(true)
            .open(&managed_path)
            .expect("managed fixture should open for deliberate tampering");
        managed
            .write_all(b"changed length")
            .expect("managed fixture should be deliberately changed");

        let error = library
            .revalidate_for_deployment(artifact.id, 1)
            .expect_err("size tampering must fail preflight");

        assert!(matches!(
            error,
            ImageLibraryError::ManagedSizeMismatch { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn deployment_revalidation_rejects_an_image_index_outside_the_wim() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("windows.wim");
        create_wim(&source_path, 2, b"two image fixture");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        let error = library
            .revalidate_for_deployment(artifact.id, 3)
            .expect_err("out-of-range image indexes must fail preflight");

        assert!(matches!(
            error,
            ImageLibraryError::ImageIndexOutOfRange {
                id,
                requested: 3,
                image_count: 2,
            } if id == artifact.id
        ));
    }

    #[test]
    fn opening_a_legacy_external_wim_downgrades_and_persists_verification() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("legacy.wim");
        let contents = create_wim(&source_path, 1, b"legacy external image");
        let source_path = fs::canonicalize(&source_path).expect("source should canonicalize");
        let library_dir = temp.path().join("library");
        fs::create_dir(&library_dir).expect("library fixture directory should be created");
        let legacy = ImageArtifact {
            id: Uuid::new_v4(),
            name: "Legacy external WIM".to_owned(),
            format: ImageFormat::Wim,
            source_path: source_path.display().to_string(),
            size_bytes: contents.len() as u64,
            sha256: Some(
                hash_files(std::slice::from_ref(&source_path)).expect("fixture should hash"),
            ),
            spans: Vec::new(),
            verified: true,
            gho_capability: None,
            created_at: chrono::Utc::now(),
        };
        let manifest = ImageManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            images: vec![legacy],
        };
        fs::write(
            library_dir.join("images.json"),
            serde_json::to_vec_pretty(&manifest).expect("fixture manifest should serialize"),
        )
        .expect("fixture manifest should write");

        let library = ImageLibrary::open(&library_dir).expect("library should open");
        let loaded = library.list().expect("images should list");

        assert_eq!(loaded.len(), 1);
        assert!(!loaded[0].verified);
        let persisted: ImageManifest = serde_json::from_slice(
            &fs::read(library_dir.join("images.json")).expect("manifest should remain readable"),
        )
        .expect("manifest should remain valid");
        assert!(!persisted.images[0].verified);
    }

    #[test]
    fn imports_and_reloads_a_catalog_only_gho_image() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows11.GHO");
        let image_contents = b"easydeploymesh-image";
        create_image(&source_path, image_contents);

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        assert_eq!(artifact.name, "Windows11");
        assert_eq!(artifact.format, ImageFormat::Gho);
        assert_eq!(artifact.size_bytes, image_contents.len() as u64);
        assert!(!artifact.verified);
        assert_eq!(artifact.sha256.as_deref().map(str::len), Some(64));

        let reloaded =
            ImageLibrary::open(temp.path().join("library")).expect("library should reload");
        assert_eq!(reloaded.list().expect("images should list"), vec![artifact]);
    }

    #[test]
    fn import_preserves_the_original_primary_basename_in_the_object_directory() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows 11.GHO");
        create_image(&source_path, b"easydeploymesh-image");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let managed_path = Path::new(&artifact.source_path);

        assert_eq!(
            managed_path.file_name().and_then(|name| name.to_str()),
            Some("Windows 11.GHO")
        );
        let object_id = managed_path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .expect("managed image should be isolated in an object directory");
        assert!(Uuid::parse_str(object_id).is_ok());
    }

    #[test]
    fn imports_three_digit_ghost_spans_for_a_long_primary_name_in_sequence_order() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows11.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windo002.ghs"), b"second span");
        create_image(&temp.path().join("Windo001.GHS"), b"first span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let span_names = artifact
            .spans
            .iter()
            .map(|span| {
                Path::new(span)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("managed span should retain its basename")
            })
            .collect::<Vec<_>>();

        assert_eq!(span_names, vec!["Windo001.GHS", "Windo002.ghs"]);
    }

    #[test]
    fn imports_ghost_spans_when_the_official_prefix_ends_in_a_digit() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("win10-enterprise.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("win10001.ghs"), b"first span");
        create_image(&temp.path().join("win10002.ghs"), b"second span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        assert_eq!(artifact.spans.len(), 2);
        assert_eq!(
            artifact
                .spans
                .iter()
                .map(|span| Path::new(span)
                    .file_name()
                    .expect("span should have a basename"))
                .collect::<Vec<_>>(),
            vec![OsStr::new("win10001.ghs"), OsStr::new("win10002.ghs")]
        );
    }

    #[test]
    fn imports_five_digit_ghost_spans_for_a_long_primary_name() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows11.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windo00001.ghs"), b"first span");
        create_image(&temp.path().join("Windo00002.ghs"), b"second span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let span_names = artifact
            .spans
            .iter()
            .map(|span| {
                Path::new(span)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("managed span should retain its basename")
            })
            .collect::<Vec<_>>();

        assert_eq!(span_names, vec!["Windo00001.ghs", "Windo00002.ghs"]);
    }

    #[test]
    fn does_not_catalog_a_nonstandard_full_stem_ghs_for_a_long_primary_name() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows11.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windows11001.ghs"), b"unrelated GHS");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        assert!(artifact.spans.is_empty());
        assert_eq!(artifact.size_bytes, b"primary".len() as u64);
    }

    #[test]
    fn imports_cns_numeric_ghost_spans_in_sequence_order() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows Backup.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windows Backup.002"), b"second span");
        create_image(&temp.path().join("Windows Backup.001"), b"first span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let span_names = artifact
            .spans
            .iter()
            .map(|span| {
                Path::new(span)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("managed span should retain its basename")
            })
            .collect::<Vec<_>>();

        assert_eq!(span_names, vec!["Windows Backup.001", "Windows Backup.002"]);
    }

    #[test]
    fn rejects_a_ghost_span_set_with_a_missing_sequence_number() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"first span");
        create_image(&temp.path().join("disk003.ghs"), b"third span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
        assert!(library.list().expect("images should list").is_empty());
    }

    #[test]
    fn rejects_mixed_three_and_five_digit_ghost_span_schemes() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"three digit");
        create_image(&temp.path().join("disk00002.ghs"), b"five digit");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
    }

    #[test]
    fn rejects_mixed_ghs_and_cns_ghost_span_schemes() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"GHS span");
        create_image(&temp.path().join("disk.002"), b"CNS span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn rejects_case_conflicting_duplicate_ghost_spans_for_windows_safety() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"first copy");
        create_image(&temp.path().join("DISK001.GHS"), b"case-conflicting copy");

        let represented_variants = fs::read_dir(temp.path())
            .expect("fixture directory should be readable")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("disk001.ghs")
            })
            .count();
        if represented_variants < 2 {
            return;
        }

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
    }

    #[test]
    fn rejects_ghost_spans_when_two_primary_images_share_the_official_prefix() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows11.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windows10.gho"), b"other primary");
        create_image(&temp.path().join("Windo001.ghs"), b"ambiguous span");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
    }

    #[cfg(not(windows))]
    #[test]
    fn rejects_a_cns_set_with_case_conflicting_primary_images() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("DISK.GHO"), b"case-conflicting primary");
        create_image(&temp.path().join("disk.001"), b"CNS span");
        let represented_primaries = fs::read_dir(temp.path())
            .expect("fixture directory should be readable")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case("disk.gho")
            })
            .count();
        if represented_primaries < 2 {
            return;
        }

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::InvalidSpanSet { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn reports_directory_entry_metadata_errors_instead_of_skipping_spans() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        std::os::unix::fs::symlink(
            temp.path().join("missing-span-target"),
            temp.path().join("disk001.ghs"),
        )
        .expect("dangling span fixture should be created");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        assert!(matches!(
            library.import(&source_path),
            Err(ImageLibraryError::Read { .. })
        ));
    }

    #[test]
    fn preserves_wim_esd_and_swm_basenames_without_changing_swm_discovery() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let wim_path = temp.path().join("Standalone.WIM");
        let esd_path = temp.path().join("Recovery.EsD");
        let swm_path = temp.path().join("install.swm");
        create_wim(&wim_path, 1, b"WIM payload");
        create_wim(&esd_path, 1, b"ESD payload");
        create_image(&swm_path, b"SWM primary");
        create_image(&temp.path().join("install2.swm"), b"SWM span two");
        create_image(&temp.path().join("install3.SWM"), b"SWM span three");
        create_image(&temp.path().join("unrelated.swm"), b"unrelated SWM");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let wim = library.import(&wim_path).expect("WIM should import");
        let esd = library.import(&esd_path).expect("ESD should import");
        let swm = library.import(&swm_path).expect("SWM should import");

        assert_eq!(
            Path::new(&wim.source_path).file_name(),
            Some(OsStr::new("Standalone.WIM"))
        );
        assert_eq!(
            Path::new(&esd.source_path).file_name(),
            Some(OsStr::new("Recovery.EsD"))
        );
        assert_eq!(
            Path::new(&swm.source_path).file_name(),
            Some(OsStr::new("install.swm"))
        );
        assert_eq!(
            swm.spans
                .iter()
                .map(|span| Path::new(span)
                    .file_name()
                    .expect("span should have a basename"))
                .collect::<Vec<_>>(),
            vec![OsStr::new("install2.swm"), OsStr::new("install3.SWM")]
        );
    }

    #[test]
    fn includes_ghost_span_files_in_size_and_digest() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"span");
        create_image(&temp.path().join("other.ghs"), b"unrelated");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        assert_eq!(artifact.size_bytes, 11);
        assert_eq!(artifact.spans.len(), 1);
        assert!(!artifact.verified);
        assert_eq!(
            fs::read(&artifact.source_path).expect("managed primary should be readable"),
            b"primary"
        );
        assert_eq!(
            fs::read(&artifact.spans[0]).expect("managed span should be readable"),
            b"span"
        );
    }

    #[test]
    fn prepares_a_managed_gho_image_set_for_read_only_provider_checks() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows Backup.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("Windows Backup.001"), b"first span");
        create_image(&temp.path().join("Windows Backup.002"), b"second span");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");

        let prepared = library
            .prepare_gho_readiness(artifact.id)
            .expect("managed GHO set should be prepared");

        assert_eq!(prepared.artifact_id, artifact.id);
        assert_eq!(prepared.primary.basename, "Windows Backup.gho");
        assert_eq!(
            prepared.primary.canonical_path,
            fs::canonicalize(&artifact.source_path).expect("managed primary should canonicalize")
        );
        assert_eq!(prepared.primary.size_bytes, 7);
        assert_eq!(
            prepared.primary.sha256,
            "986a1b7135f4986150aa5fa0028feeaa66cdaf3ed6a00a355dd86e042f7fb494"
        );
        assert_eq!(
            prepared
                .spans
                .iter()
                .map(|span| span.basename.as_str())
                .collect::<Vec<_>>(),
            vec!["Windows Backup.001", "Windows Backup.002"]
        );
        assert_eq!(prepared.spans[0].size_bytes, 10);
        assert_eq!(
            prepared.spans[0].sha256,
            "e28dabc2ca28a149f7fa4fc142a0b34950cf51a9719e5a72d49e9c1caed2c05b"
        );
        assert_eq!(prepared.spans[1].size_bytes, 11);
        assert_eq!(
            prepared.spans[1].sha256,
            "f6df6c78c3d7cd13c6fcf72a50d8899fb6cfaab602ebea90b6d5ad26d4005371"
        );
        assert_eq!(prepared.total_size_bytes, 28);
        assert_eq!(
            prepared.image_set_sha256,
            "52d8556d221892a6fa0ea63ae5ea32fd0322120b64d02d03a1e791e1c2c65c29"
        );
        assert!(
            !library
                .get(artifact.id)
                .expect("image lookup should work")
                .expect("image should remain catalogued")
                .verified
        );
    }

    #[test]
    fn gho_readiness_rejects_a_stored_span_that_does_not_belong_to_the_primary() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows Backup.gho");
        create_image(&source_path, b"primary");
        let library_dir = temp.path().join("library");
        let library = ImageLibrary::open(&library_dir).expect("library should open");
        let mut artifact = library.import(&source_path).expect("image should import");
        let object_directory = Path::new(&artifact.source_path)
            .parent()
            .expect("managed image should have an object directory");
        let unrelated_span = object_directory.join("Other001.ghs");
        create_image(&unrelated_span, b"unrelated");
        artifact.spans = vec![unrelated_span.display().to_string()];
        artifact.size_bytes = 16;
        artifact.sha256 =
            Some("1067e26e6f943cb3188767a0457285980bc55f7eec1431c2d93eea9a996db6fa".to_owned());
        persist_manifest(
            &library_dir.join("images.json"),
            std::slice::from_ref(&artifact),
        )
        .expect("deliberately malformed fixture manifest should write");
        drop(library);
        let library = ImageLibrary::open(&library_dir).expect("library should reopen");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("an unrelated GHS file must not become part of the prepared image set");

        assert!(matches!(error, ImageLibraryError::InvalidSpanSet { .. }));
    }

    #[test]
    fn gho_readiness_rejects_a_non_gho_artifact() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("Windows.wim");
        create_wim(&source_path, 1, b"deployable WIM");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("WIM should import");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("the GHO provider seam must reject WIM/ESD artifacts");

        assert!(matches!(error, ImageLibraryError::UnsupportedFormat(_)));
        assert!(
            library
                .get(artifact.id)
                .expect("image lookup should work")
                .expect("WIM should remain catalogued")
                .verified
        );
    }

    #[test]
    fn gho_readiness_rejects_same_size_span_tampering() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"span");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("GHO set should import");
        create_image(Path::new(&artifact.spans[0]), b"SPAN");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("same-size managed span tampering must fail readiness preparation");

        assert!(matches!(
            error,
            ImageLibraryError::ManagedHashMismatch { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn gho_readiness_rejects_a_changed_total_size() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("GHO should import");
        let mut managed = fs::OpenOptions::new()
            .append(true)
            .open(&artifact.source_path)
            .expect("managed fixture should open for deliberate tampering");
        managed
            .write_all(b"changed length")
            .expect("managed fixture should be deliberately changed");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("a changed aggregate size must fail readiness preparation");

        assert!(matches!(
            error,
            ImageLibraryError::ManagedSizeMismatch { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn gho_readiness_rejects_an_external_gho_path() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let external_path = temp.path().join("external.gho");
        create_image(&external_path, b"external gho");
        let external_path =
            fs::canonicalize(&external_path).expect("external fixture should canonicalize");
        let library_dir = temp.path().join("library");
        drop(ImageLibrary::open(&library_dir).expect("library should initialize"));
        let artifact = ImageArtifact {
            id: Uuid::new_v4(),
            name: "Legacy external GHO".to_owned(),
            format: ImageFormat::Gho,
            source_path: external_path.display().to_string(),
            size_bytes: 12,
            sha256: Some(
                "825c4ac87ec76def0b7b1e03dac5a63d07b6b3e3e6ec19f8a5923de9b417dce4".to_owned(),
            ),
            spans: Vec::new(),
            verified: false,
            gho_capability: None,
            created_at: chrono::Utc::now(),
        };
        persist_manifest(
            &library_dir.join("images.json"),
            std::slice::from_ref(&artifact),
        )
        .expect("legacy manifest fixture should write");
        let library = ImageLibrary::open(&library_dir).expect("library should reopen");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("external files must not cross the managed readiness seam");

        assert!(matches!(
            error,
            ImageLibraryError::UnmanagedImagePath { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn gho_readiness_rejects_a_span_from_another_managed_object() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let first_source = temp.path().join("first.gho");
        let second_source = temp.path().join("second.gho");
        create_image(&first_source, b"first primary");
        create_image(&second_source, b"second primary");
        create_image(&temp.path().join("secon001.ghs"), b"second span");
        let library_dir = temp.path().join("library");
        let library = ImageLibrary::open(&library_dir).expect("library should open");
        let mut first = library
            .import(&first_source)
            .expect("first GHO should import");
        let second = library
            .import(&second_source)
            .expect("second GHO set should import");
        first.spans = vec![second.spans[0].clone()];
        persist_manifest(&library_dir.join("images.json"), &[first.clone(), second])
            .expect("deliberately malformed fixture manifest should write");
        drop(library);
        let library = ImageLibrary::open(&library_dir).expect("library should reopen");

        let error = library
            .prepare_gho_readiness(first.id)
            .expect_err("all prepared files must share one managed object directory");

        assert!(matches!(
            error,
            ImageLibraryError::UnmanagedImagePath { id, .. } if id == first.id
        ));
    }

    #[cfg(unix)]
    #[test]
    fn gho_readiness_rejects_a_symlinked_managed_span() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        let source_span = temp.path().join("disk001.ghs");
        create_image(&source_path, b"primary");
        create_image(&source_span, b"span");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("GHO set should import");
        fs::remove_file(&artifact.spans[0]).expect("managed span fixture should be replaceable");
        std::os::unix::fs::symlink(&source_span, &artifact.spans[0])
            .expect("managed symlink fixture should be created");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("a symlink must not be exposed as a regular managed GHO span");

        assert!(matches!(
            error,
            ImageLibraryError::ManagedFileUnavailable { id, .. } if id == artifact.id
        ));
    }

    #[test]
    fn gho_readiness_rejects_a_missing_recorded_span() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("disk.gho");
        create_image(&source_path, b"primary");
        create_image(&temp.path().join("disk001.ghs"), b"span");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("GHO set should import");
        fs::remove_file(&artifact.spans[0]).expect("managed span fixture should be removable");

        let error = library
            .prepare_gho_readiness(artifact.id)
            .expect_err("a missing recorded span must fail readiness preparation");

        assert!(matches!(error, ImageLibraryError::InvalidSpanSet { .. }));
    }

    #[test]
    fn rejects_unsupported_files_without_changing_library() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("notes.txt");
        create_image(&source_path, b"not an image");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let error = library
            .import(&source_path)
            .expect_err("text must be rejected");

        assert!(matches!(error, ImageLibraryError::UnsupportedFormat(_)));
        assert!(library.list().expect("images should list").is_empty());
    }

    #[test]
    fn content_identical_images_with_different_basenames_remain_distinct() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let first_path = temp.path().join("First.gho");
        let second_path = temp.path().join("Second.gho");
        create_image(&first_path, b"same image bytes");
        create_image(&second_path, b"same image bytes");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        let first = library
            .import(&first_path)
            .expect("first image should import");
        let second = library
            .import(&second_path)
            .expect("renamed image should import independently");

        assert_ne!(first.id, second.id);
        assert_eq!(first.name, "First");
        assert_eq!(second.name, "Second");
        assert_eq!(
            Path::new(&second.source_path).file_name(),
            Some(OsStr::new("Second.gho"))
        );
        assert_eq!(library.list().expect("images should list").len(), 2);
    }

    #[test]
    fn identical_span_streams_with_different_file_boundaries_remain_distinct() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let first_dir = temp.path().join("first");
        let second_dir = temp.path().join("second");
        fs::create_dir(&first_dir).expect("first fixture directory should be created");
        fs::create_dir(&second_dir).expect("second fixture directory should be created");
        let first_path = first_dir.join("disk.gho");
        let second_path = second_dir.join("disk.gho");
        create_image(&first_path, b"ab");
        create_image(&first_dir.join("disk001.ghs"), b"cd");
        create_image(&second_path, b"a");
        create_image(&second_dir.join("disk001.ghs"), b"bcd");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");

        let first = library
            .import(&first_path)
            .expect("first image should import");
        let second = library
            .import(&second_path)
            .expect("differently split image should import independently");

        assert_ne!(first.id, second.id);
        assert_eq!(
            fs::read(&second.source_path).expect("primary should read"),
            b"a"
        );
        assert_eq!(
            fs::read(&second.spans[0]).expect("span should read"),
            b"bcd"
        );
        assert_eq!(library.list().expect("images should list").len(), 2);
    }

    #[test]
    fn importing_same_file_is_idempotent() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("base.wim");
        create_wim(&source_path, 1, b"idempotent image");

        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let first = library
            .import(&source_path)
            .expect("first import should work");
        let second = library
            .import(&source_path)
            .expect("second import should work");

        assert_eq!(first, second);
        assert_eq!(library.list().expect("images should list").len(), 1);
    }

    #[test]
    fn removing_an_image_cleans_only_its_managed_object_directory() {
        let temp = tempfile::tempdir().expect("temporary directory should be available");
        let source_path = temp.path().join("catalog.gho");
        create_image(&source_path, b"external source must survive removal");
        let library = ImageLibrary::open(temp.path().join("library")).expect("library should open");
        let artifact = library.import(&source_path).expect("image should import");
        let managed_path = PathBuf::from(&artifact.source_path);
        let managed_directory = managed_path
            .parent()
            .expect("managed image should have an object directory")
            .to_path_buf();
        assert!(managed_directory.exists());

        assert!(
            library
                .remove(artifact.id)
                .expect("image should be removed")
        );

        assert!(!managed_directory.exists());
        assert_eq!(
            fs::read(&source_path).expect("external source must not be removed"),
            b"external source must survive removal"
        );
        assert!(library.list().expect("images should list").is_empty());
    }
}
