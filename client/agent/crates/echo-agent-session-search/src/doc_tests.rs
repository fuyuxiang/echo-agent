use super::*;

fn test_session(session_id: &str, cwd: &str, title: &str) -> IndexableSession {
    IndexableSession {
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        updated_at_unix: 1_700_000_000,
        title: title.to_string(),
        updates_path: None,
    }
}

#[test]
fn test_build_session_doc_hashes_content() {
    let session = test_session("test-session", "/workspace", "My session title");

    let doc = build_session_doc(&session, "prompt text".to_string());
    assert_eq!(doc.session_id, "test-session");
    assert_eq!(doc.title, "My session title");
    assert_eq!(doc.content, "prompt text");
    assert!(!doc.content_hash.is_empty());

    // Same content + same title → same hash
    let doc2 = build_session_doc(&session, "prompt text".to_string());
    assert_eq!(doc.content_hash, doc2.content_hash);
}

#[test]
fn test_build_session_doc_title_change_changes_hash() {
    let old = test_session("s1", "/workspace", "Old title");
    let new = test_session("s1", "/workspace", "New title");
    let content = "same prompt text".to_string();

    let doc_old = build_session_doc(&old, content.clone());
    let doc_new = build_session_doc(&new, content);

    assert_ne!(
        doc_old.content_hash, doc_new.content_hash,
        "title change must produce a different hash so dedup doesn't skip the upsert"
    );
}

#[test]
fn test_should_skip_session_large_file() {
    use std::io::Write as _;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&[0u8; 1024]).unwrap();
    f.flush().unwrap();

    assert!(should_skip_session(f.path(), 512));
}

#[test]
fn test_should_skip_session_small_file() {
    use std::io::Write as _;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&[0u8; 1024]).unwrap();
    f.flush().unwrap();

    assert!(!should_skip_session(f.path(), 2048));
}

#[test]
fn test_should_skip_session_exact_limit() {
    use std::io::Write as _;
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(&[0u8; 1024]).unwrap();
    f.flush().unwrap();

    assert!(!should_skip_session(f.path(), 1024));
}

#[test]
fn test_should_skip_session_nonexistent_file() {
    assert!(!should_skip_session(
        Path::new("/nonexistent/updates.jsonl"),
        100
    ));
}
