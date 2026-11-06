//! Encryption at rest for the full-text index (which stores the extracted text of documents).
//!
//! * Cipher: ChaCha20 (IETF, 256-bit key), one random 96-bit nonce per file written to a
//!   16-byte header (`MNE1` magic + nonce), so no keystream is ever reused. Tantivy index
//!   files are append-only and written once; `meta.json` and `.managed.json` are rewritten
//!   under a fresh nonce each time. Random access works because the keystream is seekable.
//! * Not authenticated: this protects confidentiality of a copied or stolen index, not
//!   against someone who can modify files on disk.
//! * Key: 32 random bytes, wrapped with Windows DPAPI (current user) in `key.bin` in the
//!   config directory. Only that user account on that machine can unwrap it. On other
//!   platforms there is no OS key store here, so the key file is stored with mode 0600:
//!   that only deters casual copying and is documented as such.
//! * Switching the setting rebuilds the index (it is derived data); the plaintext copy is
//!   deleted, not kept as a backup.

use std::fmt;
use std::io::{self, BufWriter, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use chacha20::ChaCha20;
use chacha20::cipher::{KeyIvInit, StreamCipher, StreamCipherSeek};
use tantivy::directory::error::{DeleteError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, DirectoryLock, FileHandle, Lock, MmapDirectory, OwnedBytes, TerminatingWrite,
    WatchCallback, WatchHandle, WritePtr,
};
use tantivy::schema::Schema;
use tantivy::{Directory, HasLen, Index};

const MAGIC: &[u8; 4] = b"MNE1";
const HEADER: usize = 16;

static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
/// Encryption was requested. If the key is then unavailable we must fail closed: never
/// delete an encrypted index or write a plaintext one in its place.
static REQUIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Loads (or creates) the key when encryption is switched on. Call once at startup.
pub fn init(enabled: bool) {
    REQUIRED.store(enabled, std::sync::atomic::Ordering::SeqCst);
    let key = if enabled { load_or_create_key() } else { None };
    if enabled && key.is_none() {
        crate::crashlog::info(
            "encryption requested but the key is unavailable; index stays unencrypted",
        );
    }
    let _ = KEY.set(key);
}

pub fn enabled() -> bool {
    matches!(KEY.get(), Some(Some(_)))
}

fn locked() -> bool {
    REQUIRED.load(std::sync::atomic::Ordering::SeqCst) && key().is_none()
}

/// Sub-key for the activity database (domain-separated from the index key).
pub fn activity_key() -> Option<[u8; 32]> {
    use sha2::{Digest, Sha256};
    let k = key()?;
    let mut h = Sha256::new();
    h.update(b"mnemo-activity-v1");
    h.update(k);
    Some(h.finalize().into())
}

fn key() -> Option<[u8; 32]> {
    KEY.get().copied().flatten()
}

pub(crate) fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut out = [0u8; N];
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
        };
        // SAFETY: `out` is a valid writable buffer of N bytes; no algorithm handle is needed
        // with the system-preferred RNG flag.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                out.as_mut_ptr(),
                N as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status != 0 {
            return Err(io::Error::other("BCryptGenRandom failed"));
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut out)?;
    }
    Ok(out)
}

fn key_path() -> std::path::PathBuf {
    crate::config::MnemoConfig::config_dir().join("key.bin")
}

#[cfg(windows)]
mod dpapi {
    use std::io;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData,
    };

    const ENTROPY: &[u8] = b"mnemo-index-key-v1";

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI returned a LocalAlloc'ed buffer of `cbData` bytes that we copy and free.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn protect(data: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: all pointers refer to live blobs for the duration of the call.
        let ok = unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }

    pub fn unprotect(data: &[u8]) -> io::Result<Vec<u8>> {
        let input = blob(data);
        let entropy = blob(ENTROPY);
        let mut out = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        // SAFETY: as above.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                &entropy,
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut out,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(take(out))
    }
}

fn load_or_create_key() -> Option<[u8; 32]> {
    let path = key_path();
    #[cfg(windows)]
    {
        if let Ok(wrapped) = std::fs::read(&path) {
            // A key that cannot be unwrapped (other user, other machine) is NOT replaced:
            // doing so would silently orphan the encrypted index.
            return dpapi::unprotect(&wrapped)
                .ok()
                .and_then(|k| k.try_into().ok());
        }
        let key = random_bytes::<32>().ok()?;
        let wrapped = dpapi::protect(&key).ok()?;
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, wrapped).ok()?;
        Some(key)
    }
    #[cfg(not(windows))]
    {
        if let Ok(raw) = std::fs::read(&path) {
            return raw.try_into().ok();
        }
        let key = random_bytes::<32>().ok()?;
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, key).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Some(key)
    }
}

// ---------------------------------------------------------------------------------------
// Encrypted tantivy directory
// ---------------------------------------------------------------------------------------

#[derive(Clone)]
struct EncDirectory {
    inner: MmapDirectory,
    key: [u8; 32],
}

impl fmt::Debug for EncDirectory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "EncDirectory({:?})", self.inner)
    }
}

fn cipher(key: &[u8; 32], nonce: &[u8; 12]) -> ChaCha20 {
    ChaCha20::new(key.into(), nonce.into())
}

fn header(nonce: &[u8; 12]) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..4].copy_from_slice(MAGIC);
    h[4..].copy_from_slice(nonce);
    h
}

fn parse_header(bytes: &[u8]) -> io::Result<[u8; 12]> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an encrypted index file",
        ));
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes[4..HEADER]);
    Ok(nonce)
}

#[derive(Debug)]
struct EncHandle {
    inner: Arc<dyn FileHandle>,
    key: [u8; 32],
    nonce: [u8; 12],
}

impl HasLen for EncHandle {
    fn len(&self) -> usize {
        self.inner.len().saturating_sub(HEADER)
    }
}

impl FileHandle for EncHandle {
    fn read_bytes(&self, range: Range<usize>) -> io::Result<OwnedBytes> {
        let raw = self
            .inner
            .read_bytes(range.start + HEADER..range.end + HEADER)?;
        let mut buf = raw.as_slice().to_vec();
        let mut c = cipher(&self.key, &self.nonce);
        c.seek(range.start as u64);
        c.apply_keystream(&mut buf);
        Ok(OwnedBytes::new(buf))
    }
}

struct EncWriter {
    inner: WritePtr,
    cipher: ChaCha20,
}

impl Write for EncWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut tmp = buf.to_vec();
        self.cipher.apply_keystream(&mut tmp);
        self.inner.write_all(&tmp)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl TerminatingWrite for EncWriter {
    fn terminate_ref(&mut self, token: AntiCallToken) -> io::Result<()> {
        self.inner.flush()?;
        self.inner.get_mut().terminate_ref(token)
    }
}

impl Directory for EncDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        let inner = self.inner.get_file_handle(path)?;
        let head = inner
            .read_bytes(0..HEADER)
            .map_err(|e| OpenReadError::wrap_io_error(e, path.to_path_buf()))?;
        let nonce = parse_header(head.as_slice())
            .map_err(|e| OpenReadError::wrap_io_error(e, path.to_path_buf()))?;
        Ok(Arc::new(EncHandle {
            inner,
            key: self.key,
            nonce,
        }))
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let mut inner = self.inner.open_write(path)?;
        let nonce = random_bytes::<12>()
            .map_err(|e| OpenWriteError::wrap_io_error(e, path.to_path_buf()))?;
        inner
            .write_all(&header(&nonce))
            .map_err(|e| OpenWriteError::wrap_io_error(e, path.to_path_buf()))?;
        let writer = EncWriter {
            inner,
            cipher: cipher(&self.key, &nonce),
        };
        Ok(BufWriter::with_capacity(64 * 1024, Box::new(writer)))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        let raw = self.inner.atomic_read(path)?;
        let nonce =
            parse_header(&raw).map_err(|e| OpenReadError::wrap_io_error(e, path.to_path_buf()))?;
        let mut data = raw[HEADER..].to_vec();
        cipher(&self.key, &nonce).apply_keystream(&mut data);
        Ok(data)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let nonce = random_bytes::<12>()?;
        let mut out = header(&nonce).to_vec();
        let mut body = data.to_vec();
        cipher(&self.key, &nonce).apply_keystream(&mut body);
        out.extend_from_slice(&body);
        self.inner.atomic_write(path, &out)
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }

    fn acquire_lock(
        &self,
        lock: &Lock,
    ) -> Result<DirectoryLock, tantivy::directory::error::LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, cb: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(cb)
    }
}

/// True when `dir` holds an index whose format does not match the current setting
/// (plaintext while encryption is on, or encrypted while it is off).
pub fn format_mismatch(dir: &Path) -> bool {
    if locked() {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(dir.join("meta.json")) else {
        return false;
    };
    let mut head = [0u8; 4];
    let encrypted = std::io::Read::read_exact(&mut f, &mut head).is_ok() && &head == MAGIC;
    encrypted != enabled()
}

fn enc_dir(dir: &Path) -> io::Result<EncDirectory> {
    let key = key().ok_or_else(|| io::Error::other("encryption key unavailable"))?;
    let inner = MmapDirectory::open(dir).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(EncDirectory { inner, key })
}

pub fn open_index(dir: &Path) -> tantivy::Result<Index> {
    if locked() {
        return Err(tantivy::TantivyError::SystemError(
            "encryption key unavailable".into(),
        ));
    }
    if enabled() {
        let d = enc_dir(dir).map_err(|e| tantivy::TantivyError::SystemError(e.to_string()))?;
        Index::open(d)
    } else {
        Index::open_in_dir(dir)
    }
}

pub fn create_index(dir: &Path, schema: Schema) -> tantivy::Result<Index> {
    if locked() {
        return Err(tantivy::TantivyError::SystemError(
            "encryption key unavailable".into(),
        ));
    }
    if enabled() {
        let d = enc_dir(dir).map_err(|e| tantivy::TantivyError::SystemError(e.to_string()))?;
        Index::create(d, schema, tantivy::IndexSettings::default())
    } else {
        Index::create_in_dir(dir, schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tantivy::collector::Count;
    use tantivy::query::QueryParser;
    use tantivy::schema::{STORED, TEXT};

    #[test]
    fn encrypted_index_roundtrips_and_hides_plaintext() {
        let dir = tempfile::tempdir().unwrap();
        // Uses the directory directly: the process-wide key is shared by other tests.
        let enc = || EncDirectory {
            inner: MmapDirectory::open(dir.path()).unwrap(),
            key: [7u8; 32],
        };
        let mut sb = Schema::builder();
        let body = sb.add_text_field("body", TEXT | STORED);
        let schema = sb.build();
        let index = Index::create(enc(), schema, tantivy::IndexSettings::default()).unwrap();
        let mut w = index.writer(15_000_000).unwrap();
        let secret = "zebrafish passphrase quokka";
        w.add_document(tantivy::doc!(body => secret)).unwrap();
        w.commit().unwrap();
        drop(w);

        let reopened = Index::open(enc()).unwrap();
        let reader = reopened.reader().unwrap();
        let q = QueryParser::for_index(&reopened, vec![body])
            .parse_query("quokka")
            .unwrap();
        assert_eq!(reader.searcher().search(&q, &Count).unwrap(), 1);

        // No file on disk contains the plaintext, and the format check sees encryption.
        for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            assert!(
                !bytes.windows(6).any(|w| w == b"quokka"),
                "plaintext in {:?}",
                entry.path()
            );
        }
        let meta = std::fs::read(dir.path().join("meta.json")).unwrap();
        assert_eq!(&meta[..4], MAGIC);
    }
}
