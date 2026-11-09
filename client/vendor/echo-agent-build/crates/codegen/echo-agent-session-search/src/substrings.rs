//! Exact, ASCII-case-insensitive substring search alongside the word index.
//!
//! SQLite's trigram tokenizer needs three characters. Separating each input
//! character and padding both ends gives even a single character a trigram.
//! An encoded phrase matches across neither characters nor fields by accident.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const BATCH_SIZE: usize = 8;
const SEPARATOR: char = '\u{1f}';
const ESCAPE: char = '\u{1e}';

fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for character in text.chars() {
        encoded.push(SEPARATOR);
        match character {
            SEPARATOR => encoded.push_str("\u{1e}1"),
            ESCAPE => encoded.push_str("\u{1e}2"),
            '\0' => encoded.push_str("\u{1e}0"),
            other => encoded.push(other.to_ascii_lowercase()),
        }
    }
    encoded.push(SEPARATOR);
    encoded
}

pub(super) fn match_query(terms: &[String], operator: &str) -> String {
    terms
        .iter()
        .map(|term| format!("\"{}\"", encode(term).replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(operator)
}

pub(super) fn reset(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "DROP TRIGGER IF EXISTS session_substrings_ai;
         DROP TRIGGER IF EXISTS session_substrings_au;
         DROP TRIGGER IF EXISTS session_substrings_ad;
         DROP TABLE IF EXISTS session_substrings;
         DROP TABLE IF EXISTS session_substrings_dirty;",
    )
}

fn initialized(db: &Connection) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'session_substrings_dirty')",
        [],
        |row| row.get(0),
    )
}

pub(super) fn initialize(db: &mut Connection) -> rusqlite::Result<()> {
    if initialized(db)? {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !initialized(&tx)? {
        // No application-defined SQL functions in these triggers. Older
        // writers still update the ordinary index and enqueue their changes.
        tx.execute_batch(
            "CREATE VIRTUAL TABLE session_substrings USING fts5(
                title, content, content='', contentless_delete=1,
                tokenize='trigram case_sensitive 1'
             );
             CREATE TABLE session_substrings_dirty (doc_rowid INTEGER PRIMARY KEY);
             CREATE TRIGGER session_substrings_ai AFTER INSERT ON session_docs BEGIN
                INSERT INTO session_substrings_dirty VALUES (new.rowid) ON CONFLICT DO NOTHING;
             END;
             CREATE TRIGGER session_substrings_au AFTER UPDATE ON session_docs BEGIN
                INSERT INTO session_substrings_dirty VALUES (old.rowid) ON CONFLICT DO NOTHING;
                INSERT INTO session_substrings_dirty VALUES (new.rowid) ON CONFLICT DO NOTHING;
             END;
             CREATE TRIGGER session_substrings_ad AFTER DELETE ON session_docs BEGIN
                INSERT INTO session_substrings_dirty VALUES (old.rowid) ON CONFLICT DO NOTHING;
             END;
             INSERT INTO session_substrings_dirty SELECT rowid FROM session_docs;",
        )?;
    }
    tx.commit()
}

pub(super) fn replace(
    db: &Connection,
    rowid: i64,
    document: Option<(&str, &str)>,
) -> rusqlite::Result<()> {
    db.execute("DELETE FROM session_substrings WHERE rowid = ?1", [rowid])?;
    if let Some((title, content)) = document {
        db.execute(
            "INSERT INTO session_substrings(rowid, title, content) VALUES (?1, ?2, ?3)",
            params![rowid, encode(title), encode(content)],
        )?;
    }
    db.execute(
        "DELETE FROM session_substrings_dirty WHERE doc_rowid = ?1",
        [rowid],
    )?;
    Ok(())
}

pub(super) fn pending(db: &Connection) -> rusqlite::Result<bool> {
    db.query_row("SELECT 1 FROM session_substrings_dirty LIMIT 1", [], |_| {
        Ok(())
    })
    .optional()
    .map(|row| row.is_some())
}

pub(super) fn refresh_batch(db: &Connection) -> rusqlite::Result<bool> {
    if !pending(db)? {
        return Ok(false);
    }
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let dirty = tx
        .prepare(
            "SELECT pending.doc_rowid, d.title, d.content FROM session_substrings_dirty pending
         LEFT JOIN session_docs d ON d.rowid = pending.doc_rowid
         ORDER BY pending.doc_rowid LIMIT ?1",
        )?
        .query_map([BATCH_SIZE], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for (rowid, title, content) in &dirty {
        replace(&tx, *rowid, title.as_deref().zip(content.as_deref()))?;
    }
    let more = pending(&tx)?;
    tx.commit()?;
    Ok(more)
}
