//! End to end: with encryption on, the content index works and no file under it holds the
//! indexed text. Runs in its own process, so the process-wide key does not leak into other tests.

use std::path::Path;

use mnemo_lib::indexer::content::ContentIndex;

#[test]
fn content_index_is_encrypted_on_disk_and_searchable() {
    let data = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded test process; set before any other code reads it.
    unsafe { std::env::set_var("MNEMO_DATA_DIR", data.path()) };
    mnemo_lib::crypt::init(true);
    assert!(
        mnemo_lib::crypt::enabled(),
        "key should be created and wrapped"
    );

    let dir = data.path().join("index");
    std::fs::create_dir_all(&dir).unwrap();
    let index = ContentIndex::new(&dir).expect("create encrypted index");
    index
        .add_or_update(
            Path::new("/docs/plan.txt"),
            "plan.txt",
            "the quokkazebra merger is confidential",
            0,
            0,
            Some("txt"),
        )
        .unwrap();
    index.commit().unwrap();
    assert_eq!(index.search_folded("quokkazebra", 10, None).len(), 1);
    drop(index);

    // Reopen from disk and search again.
    let again = ContentIndex::new(&dir).expect("reopen encrypted index");
    assert_eq!(again.search_folded("quokkazebra", 10, None).len(), 1);
    drop(again);

    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let bytes = std::fs::read(entry.path()).unwrap_or_default();
        assert!(
            !bytes.windows(10).any(|w| w == b"quokkazebr"),
            "plaintext found in {:?}",
            entry.path()
        );
    }
    assert!(data.path().join("key.bin").exists());
}
