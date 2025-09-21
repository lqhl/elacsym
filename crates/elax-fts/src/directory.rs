use std::{
    fmt,
    io::{self, BufWriter},
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Error as AnyhowError;
use bytes::Bytes;
use elax_cache::{AssetKind, Cache, CacheKey};
use object_store::{path::Path as ObjectPath, Error as ObjectStoreError, ObjectStore};
use ownedbytes::StableDeref;
use tantivy::directory::error::{DeleteError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, FileHandle, OwnedBytes, TerminatingWrite, WatchCallback, WatchCallbackList,
    WatchHandle, WritePtr,
};
use tantivy::{Directory, HasLen, Result as TantivyResult};
use tokio::runtime::Handle;

/// Tantivy [`Directory`] that materializes files from an [`ObjectStore`] and keeps hot segments in
/// the NVMe-backed [`Cache`].
#[derive(Clone)]
pub struct ObjectStoreDirectory {
    store: Arc<dyn ObjectStore>,
    root: ObjectPath,
    cache: Arc<Cache>,
    namespace: String,
    runtime: Handle,
    watchers: Arc<WatchCallbackList>,
}

impl ObjectStoreDirectory {
    /// Create a new directory rooted at `root` inside the provided `object_store`.
    pub fn new(
        store: Arc<dyn ObjectStore>,
        root: ObjectPath,
        cache: Arc<Cache>,
        namespace: impl Into<String>,
        runtime: Handle,
    ) -> Self {
        Self {
            store,
            root,
            cache,
            namespace: namespace.into(),
            runtime,
            watchers: Arc::new(WatchCallbackList::default()),
        }
    }

    fn object_path(&self, path: &Path) -> ObjectPath {
        let joined = if path.components().next().is_none() {
            self.root.clone()
        } else {
            let relative = path.to_string_lossy().replace('\\', "/");
            self.root.child(relative.as_str())
        };
        joined
    }

    fn cache_key(&self, path: &Path) -> CacheKey {
        CacheKey::new(
            self.namespace.clone(),
            path.to_string_lossy().to_string(),
            AssetKind::TantivySegment,
        )
    }

    fn load_bytes(&self, path: &Path) -> Result<Arc<Vec<u8>>, OpenReadError> {
        let key = self.cache_key(path);
        if let Some(bytes) = self
            .cache
            .get(&key)
            .map_err(|err| OpenReadError::wrap_io_error(anyhow_to_io(err), path.to_path_buf()))?
        {
            return Ok(bytes);
        }

        let object_path = self.object_path(path);
        let get_result = self
            .runtime
            .block_on(self.store.get(&object_path))
            .map_err(|err| map_object_error(path, err))?;
        let bytes = self
            .runtime
            .block_on(get_result.bytes())
            .map_err(|err| map_object_error(path, err))?;
        let arc = Arc::new(bytes.to_vec());
        self.cache
            .insert(key, arc.clone())
            .map_err(|err| OpenReadError::wrap_io_error(anyhow_to_io(err), path.to_path_buf()))?;
        Ok(arc)
    }

    fn store_bytes(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let object_path = self.object_path(path);
        let payload = Bytes::copy_from_slice(data);
        self.runtime
            .block_on(self.store.put(&object_path, payload))
            .map_err(object_error_to_io)
    }

    fn populate_cache(&self, path: &Path, data: Vec<u8>) -> io::Result<()> {
        let key = self.cache_key(path);
        self.cache.insert(key, Arc::new(data)).map_err(anyhow_to_io)
    }
}

impl Directory for ObjectStoreDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        let bytes = self.load_bytes(path)?;
        Ok(Arc::new(CachedFileHandle { bytes }))
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        let object_path = self.object_path(path);
        match self.runtime.block_on(self.store.delete(&object_path)) {
            Ok(_) => {}
            Err(ObjectStoreError::NotFound { .. }) => {
                return Err(DeleteError::FileDoesNotExist(path.to_path_buf()));
            }
            Err(err) => {
                return Err(DeleteError::IoError {
                    io_error: Arc::new(object_error_to_io(err)),
                    filepath: path.to_path_buf(),
                });
            }
        }

        let key = self.cache_key(path);
        if let Err(err) = self.cache.remove(&key) {
            let io_err = anyhow_to_io(err);
            return Err(DeleteError::IoError {
                io_error: Arc::new(io_err),
                filepath: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        let object_path = self.object_path(path);
        match self.runtime.block_on(self.store.head(&object_path)) {
            Ok(_) => Ok(true),
            Err(ObjectStoreError::NotFound { .. }) => Ok(false),
            Err(err) => Err(OpenReadError::wrap_io_error(
                object_error_to_io(err),
                path.to_path_buf(),
            )),
        }
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        if self.exists(path).map_err(|err| {
            OpenWriteError::wrap_io_error(open_read_to_io(err), path.to_path_buf())
        })? {
            return Err(OpenWriteError::FileAlreadyExists(path.to_path_buf()));
        }
        let writer = ObjectStoreWrite::new(self.clone(), path.to_path_buf());
        Ok(BufWriter::new(Box::new(writer)))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        let bytes = self.load_bytes(path)?;
        Ok((*bytes).clone())
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.store_bytes(path, data)?;
        self.populate_cache(path, data.to_vec())?;
        if path == Path::new("meta.json") {
            let _ = self.watchers.broadcast().wait();
        }
        Ok(())
    }

    fn sync_directory(&self) -> io::Result<()> {
        Ok(())
    }

    fn watch(&self, watch_callback: WatchCallback) -> TantivyResult<WatchHandle> {
        Ok(self.watchers.subscribe(watch_callback))
    }
}

impl fmt::Debug for ObjectStoreDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStoreDirectory")
            .field("root", &self.root)
            .field("namespace", &self.namespace)
            .finish()
    }
}

#[derive(Clone)]
struct CachedFileHandle {
    bytes: Arc<Vec<u8>>,
}

#[derive(Clone)]
struct CachedBytes(Arc<Vec<u8>>);

impl Deref for CachedBytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.0.as_slice()
    }
}

unsafe impl StableDeref for CachedBytes {}

impl fmt::Debug for CachedFileHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CachedFileHandle(len={})", self.bytes.len())
    }
}

impl HasLen for CachedFileHandle {
    fn len(&self) -> usize {
        self.bytes.len()
    }
}

impl FileHandle for CachedFileHandle {
    fn read_bytes(&self, range: std::ops::Range<usize>) -> io::Result<OwnedBytes> {
        if range.end > self.bytes.len() || range.start > range.end {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid range"));
        }
        let owned = OwnedBytes::new(CachedBytes(self.bytes.clone()));
        Ok(owned.slice(range))
    }
}

struct ObjectStoreWrite {
    directory: ObjectStoreDirectory,
    path: PathBuf,
    buffer: Vec<u8>,
}

impl ObjectStoreWrite {
    fn new(directory: ObjectStoreDirectory, path: PathBuf) -> Self {
        Self {
            directory,
            path,
            buffer: Vec::new(),
        }
    }
}

impl io::Write for ObjectStoreWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.directory.store_bytes(&self.path, &self.buffer)?;
        self.directory
            .populate_cache(&self.path, self.buffer.clone())?;
        Ok(())
    }
}

impl TerminatingWrite for ObjectStoreWrite {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        io::Write::flush(self)
    }
}

fn map_object_error(path: &Path, err: ObjectStoreError) -> OpenReadError {
    match err {
        ObjectStoreError::NotFound { .. } => OpenReadError::FileDoesNotExist(path.to_path_buf()),
        other => OpenReadError::wrap_io_error(object_error_to_io(other), path.to_path_buf()),
    }
}

fn object_error_to_io(err: ObjectStoreError) -> io::Error {
    match err {
        ObjectStoreError::NotFound { .. } => {
            io::Error::new(io::ErrorKind::NotFound, err.to_string())
        }
        other => io::Error::other(other.to_string()),
    }
}

fn anyhow_to_io(err: AnyhowError) -> io::Error {
    match err.downcast::<io::Error>() {
        Ok(io_err) => io_err,
        Err(other) => io::Error::other(other.to_string()),
    }
}

fn open_read_to_io(err: OpenReadError) -> io::Error {
    match err {
        OpenReadError::FileDoesNotExist(_) => {
            io::Error::new(io::ErrorKind::NotFound, err.to_string())
        }
        OpenReadError::IoError { io_error, .. } => {
            io::Error::new(io_error.kind(), io_error.to_string())
        }
        OpenReadError::IncompatibleIndex(_) => io::Error::other(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use elax_cache::CacheConfig;
    use object_store::memory::InMemory;
    use std::io::Write;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    fn test_directory(
        namespace: &str,
    ) -> (ObjectStoreDirectory, Arc<Cache>, tokio::runtime::Runtime) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let handle = runtime.handle().clone();
        let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let temp = TempDir::new().expect("tempdir");
        let cache = Arc::new(
            Cache::new(CacheConfig::new(temp.path(), 2 * 1024 * 1024, 4096)).expect("cache"),
        );
        let dir = ObjectStoreDirectory::new(
            store,
            ObjectPath::from("parts/part-1/fts"),
            cache.clone(),
            namespace,
            handle,
        );
        (dir, cache, runtime)
    }

    #[test]
    fn open_write_persists_and_caches() {
        let (dir, cache, _rt) = test_directory("ns-a");
        let path = Path::new("segment.fast");

        {
            let mut writer = dir.open_write(path).expect("open write");
            writer.write_all(b"hello tantivy").expect("write");
            writer.flush().expect("flush");
        }

        let handle = dir
            .get_file_handle(path)
            .expect("file handle from object store");
        let expected = b"hello tantivy";
        let bytes = handle
            .read_bytes(0..expected.len())
            .expect("read bytes from cached handle");
        assert_eq!(bytes.as_slice(), expected);

        let key = dir.cache_key(path);
        let cached = cache.get(&key).expect("cache lookup").expect("entry");
        assert_eq!(cached.as_slice(), b"hello tantivy");
        assert_eq!(cache.stats().entry_count, 1);
    }

    #[test]
    fn delete_clears_cache() {
        let (dir, cache, _rt) = test_directory("ns-b");
        let path = Path::new("segment.idx");

        {
            let mut writer = dir.open_write(path).unwrap();
            writer.write_all(b"data").unwrap();
            writer.flush().unwrap();
        }

        let key = dir.cache_key(path);
        assert!(cache.get(&key).unwrap().is_some());
        dir.delete(path).expect("delete segment");
        assert!(cache.get(&key).unwrap().is_none());
    }

    #[test]
    fn atomic_write_notifies_watchers() {
        let (dir, _cache, _rt) = test_directory("ns-c");
        let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        let handle = dir
            .watch(WatchCallback::new(move || {
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }))
            .expect("watch registration");

        dir.atomic_write(Path::new("meta.json"), b"v1").unwrap();
        dir.atomic_write(Path::new("meta.json"), b"v2").unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        drop(handle);
    }
}
