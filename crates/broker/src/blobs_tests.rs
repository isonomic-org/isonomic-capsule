// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

struct Store {
    _dir: tempfile::TempDir,
    root: PathBuf,
    blobs: Blobs,
}

fn store() -> Store {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let blobs = Blobs::open(&root.join("blobs"), &root.join("registry.db")).unwrap();
    Store {
        _dir: dir,
        root,
        blobs,
    }
}

const APP: Option<&str> = Some("com.isonomic.hello");
const OTHER: Option<&str> = Some("com.isonomic.other");

fn put(blobs: &Blobs, app: Option<&str>, data: &[u8]) -> BlobInfo {
    let call = Call::BlobsPut {
        content_type: "text/plain".into(),
        data: Bytes(data.to_vec()),
    };
    match blobs.answer(app, call).unwrap() {
        Reply::Blob(info) => info,
        other => panic!("{other:?}"),
    }
}

fn get(blobs: &Blobs, app: Option<&str>, handle: &str, offset: u64) -> Result<Vec<u8>, ErrorCode> {
    let call = Call::BlobsGet {
        handle: handle.into(),
        offset,
        length: None,
    };
    match blobs.answer(app, call) {
        Ok(Reply::BlobData { data, .. }) => Ok(data.0),
        Ok(other) => panic!("{other:?}"),
        Err(error) => Err(error.code),
    }
}

fn write(blobs: &Blobs, handle: &str, offset: u64, data: &[u8]) -> Result<BlobInfo, ErrorCode> {
    let call = Call::BlobsWrite {
        handle: handle.into(),
        offset,
        data: Bytes(data.to_vec()),
    };
    match blobs.answer(APP, call) {
        Ok(Reply::Blob(info)) => Ok(info),
        Ok(other) => panic!("{other:?}"),
        Err(error) => Err(error.code),
    }
}

fn create(blobs: &Blobs, size: u64) -> Result<BlobInfo, ErrorCode> {
    let call = Call::BlobsCreate {
        content_type: "image/png".into(),
        size,
    };
    match blobs.answer(APP, call) {
        Ok(Reply::Blob(info)) => Ok(info),
        Ok(other) => panic!("{other:?}"),
        Err(error) => Err(error.code),
    }
}

#[test]
fn a_put_reads_back_whole_and_in_ranges() {
    let s = store();
    let info = put(&s.blobs, APP, b"hello");
    assert_eq!((info.size, info.written), (5, 5));
    assert!(info.handle.starts_with("blob-") && info.handle.len() > 20);
    assert_eq!(get(&s.blobs, APP, &info.handle, 0).unwrap(), b"hello");
    let range = Call::BlobsGet {
        handle: info.handle.clone(),
        offset: 1,
        length: Some(3),
    };
    let Ok(Reply::BlobData { offset, data, .. }) = s.blobs.answer(APP, range) else {
        panic!()
    };
    assert_eq!((offset, data.0.as_slice()), (1, &b"ell"[..]));
    assert_eq!(get(&s.blobs, APP, &info.handle, 9).unwrap(), b"");
}

#[test]
fn a_large_blob_is_written_in_order_and_read_only_when_complete() {
    let s = store();
    let size = CHUNK_BYTES + 10;
    let info = create(&s.blobs, size).unwrap();
    let first = vec![7u8; CHUNK_BYTES as usize];
    assert_eq!(
        get(&s.blobs, APP, &info.handle, 0),
        Err(ErrorCode::Incomplete)
    );
    assert_eq!(
        write(&s.blobs, &info.handle, 5, b"x"),
        Err(ErrorCode::BadRequest)
    );
    assert_eq!(
        write(&s.blobs, &info.handle, 0, &first).unwrap().written,
        CHUNK_BYTES
    );
    assert_eq!(
        write(&s.blobs, &info.handle, CHUNK_BYTES, &[1; 11]),
        Err(ErrorCode::TooLarge),
        "a write past the declared size"
    );
    let done = write(&s.blobs, &info.handle, CHUNK_BYTES, &[1; 10]).unwrap();
    assert!(done.complete());
    assert_eq!(get(&s.blobs, APP, &info.handle, 0).unwrap(), first);
    assert_eq!(
        get(&s.blobs, APP, &info.handle, CHUNK_BYTES).unwrap(),
        [1; 10]
    );
}

#[test]
fn another_apps_handle_is_unknown_and_the_shell_reaches_every_blob() {
    let s = store();
    let info = put(&s.blobs, APP, b"mine");
    assert_eq!(
        get(&s.blobs, OTHER, &info.handle, 0),
        Err(ErrorCode::UnknownBlob)
    );
    let delete = Call::BlobsDelete {
        handle: info.handle.clone(),
    };
    assert_eq!(
        s.blobs.answer(OTHER, delete.clone()).unwrap_err().code,
        ErrorCode::UnknownBlob
    );
    assert_eq!(get(&s.blobs, None, &info.handle, 0).unwrap(), b"mine");
    assert!(matches!(s.blobs.answer(APP, delete), Ok(Reply::Deleted {})));
    assert_eq!(
        get(&s.blobs, APP, &info.handle, 0),
        Err(ErrorCode::UnknownBlob)
    );
    assert!(!s.root.join("blobs").join(&info.handle).exists());
}

#[test]
fn the_engines_limit_applies_to_puts_and_creates() {
    let s = store();
    let blobs = s.blobs.with_limit(4);
    let call = Call::BlobsPut {
        content_type: "text/plain".into(),
        data: Bytes(b"hello".to_vec()),
    };
    assert_eq!(
        blobs.answer(APP, call).unwrap_err().code,
        ErrorCode::TooLarge
    );
    assert_eq!(create(&blobs, 5), Err(ErrorCode::TooLarge));
    assert!(create(&blobs, 4).is_ok());
}

#[test]
fn a_reopened_store_resumes_where_the_count_says_and_drops_stray_bytes() {
    let s = store();
    let info = create(&s.blobs, 6).unwrap();
    write(&s.blobs, &info.handle, 0, b"abc").unwrap();
    // A crash after the bytes but before the count: three stray bytes.
    let path = s.root.join("blobs").join(&info.handle);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"zzz").unwrap();
    drop(s.blobs);

    let blobs = Blobs::open(&s.root.join("blobs"), &s.root.join("registry.db")).unwrap();
    let call = Call::BlobsInfo {
        handle: info.handle.clone(),
    };
    let Ok(Reply::Blob(again)) = blobs.answer(APP, call) else {
        panic!()
    };
    assert_eq!(again.written, 3);
    write(&blobs, &info.handle, 3, b"def").unwrap();
    assert_eq!(get(&blobs, APP, &info.handle, 0).unwrap(), b"abcdef");
}
