//! The SQLite store behaves exactly like the harness's own in-memory store,
//! and keeps what it was given across a reopen.

use harness_log::MemoryStore;

use super::*;

fn session(id: &str) -> SessionId {
    serde_json::from_value(serde_json::json!(id)).unwrap()
}

fn app(id: &str) -> AppId {
    serde_json::from_value(serde_json::json!(id)).unwrap()
}

fn event(session: &str, seq: Seq) -> SessionEvent {
    serde_json::from_value(serde_json::json!({
        "session": session, "seq": seq, "at": 1794646800000u64 + seq,
        "turn": "trn_0001", "type": "turn.start", "cause": "user"
    }))
    .unwrap()
}

fn events(session: &str, seqs: std::ops::RangeInclusive<Seq>) -> Vec<SessionEvent> {
    seqs.map(|seq| event(session, seq)).collect()
}

fn meta(id: &str, owner: &str, title: Option<&str>) -> SessionMeta {
    serde_json::from_value(serde_json::json!({
        "id": id, "app": owner, "class": "interactive",
        "preset": {"id": "chat", "version": 1}, "title": title,
        "status": "active", "archived": false, "forked_from": null,
        "created_at": 1, "updated_at": 2
    }))
    .unwrap()
}

/// Run the same calls on both stores and compare every answer.
struct Both {
    sqlite: SqliteSessionStore,
    memory: MemoryStore,
}

impl Both {
    fn new() -> Self {
        Self {
            sqlite: SqliteSessionStore::in_memory().unwrap(),
            memory: MemoryStore::new(),
        }
    }

    async fn append(&self, id: &str, events: &[SessionEvent]) -> Result<(), StoreError> {
        let s = session(id);
        let theirs = self.memory.append(&s, events).await;
        let ours = self.sqlite.append(&s, events).await;
        assert_eq!(ours, theirs, "append {id}");
        ours
    }

    async fn read(
        &self,
        id: &str,
        after: Seq,
        limit: usize,
    ) -> Result<Vec<SessionEvent>, StoreError> {
        let s = session(id);
        let theirs = self.memory.read(&s, after, limit).await;
        let ours = self.sqlite.read(&s, after, limit).await;
        assert_eq!(ours, theirs, "read {id} after {after} limit {limit}");
        ours
    }

    async fn list(&self, owner: Option<&str>) -> Vec<SessionMeta> {
        let owner = owner.map(app);
        let theirs = self.memory.list_meta(owner.as_ref()).await.unwrap();
        let ours = self.sqlite.list_meta(owner.as_ref()).await.unwrap();
        assert_eq!(ours, theirs);
        ours
    }

    async fn put(&self, meta: &SessionMeta) {
        self.memory.put_meta(meta).await.unwrap();
        self.sqlite.put_meta(meta).await.unwrap();
    }

    async fn purge(&self, ids: &[&str]) -> u64 {
        let ids: Vec<_> = ids.iter().map(|id| session(id)).collect();
        let theirs = self.memory.purge(&ids).await.unwrap();
        let ours = self.sqlite.purge(&ids).await.unwrap();
        assert_eq!(ours, theirs);
        ours
    }
}

#[tokio::test]
async fn appends_continue_the_sequence_and_refuse_gaps_and_overwrites() {
    let both = Both::new();
    assert_eq!(both.read("ses_1", 0, 10).await, Err(StoreError::NotFound));
    both.append("ses_1", &events("ses_1", 1..=3)).await.unwrap();
    both.append("ses_1", &events("ses_1", 4..=4)).await.unwrap();

    let gap = both.append("ses_1", &events("ses_1", 6..=6)).await;
    assert_eq!(
        gap,
        Err(StoreError::Conflict {
            expected: 5,
            found: 6
        })
    );
    let overwrite = both.append("ses_1", &events("ses_1", 2..=2)).await;
    assert_eq!(
        overwrite,
        Err(StoreError::Conflict {
            expected: 5,
            found: 2
        })
    );
    let elsewhere = both.append("ses_1", &events("ses_2", 5..=5)).await;
    assert!(matches!(elsewhere, Err(StoreError::Conflict { .. })));

    assert_eq!(both.read("ses_1", 0, 100).await.unwrap().len(), 4);
}

#[tokio::test]
async fn a_refused_batch_writes_nothing() {
    let both = Both::new();
    both.append("ses_1", &events("ses_1", 1..=2)).await.unwrap();
    let mut batch = events("ses_1", 3..=4);
    batch.push(event("ses_1", 9));
    assert!(both.append("ses_1", &batch).await.is_err());
    assert_eq!(both.read("ses_1", 0, 100).await.unwrap().len(), 2);
}

#[tokio::test]
async fn reads_page_in_order_after_a_seq() {
    let both = Both::new();
    both.append("ses_1", &events("ses_1", 1..=10))
        .await
        .unwrap();
    for (after, limit) in [(0, 3), (3, 3), (8, 5), (10, 5), (12, 5), (0, 0)] {
        both.read("ses_1", after, limit).await.unwrap();
    }
    let page = both.read("ses_1", 4, 2).await.unwrap();
    assert_eq!(page.iter().map(|e| e.seq).collect::<Vec<_>>(), [5, 6]);
}

#[tokio::test]
async fn registry_rows_are_replaced_listed_by_owner_and_purged_with_the_log() {
    let both = Both::new();
    both.put(&meta("ses_1", "chat", None)).await;
    both.put(&meta("ses_2", "notes", None)).await;
    both.put(&meta("ses_1", "chat", Some("Book club"))).await;
    assert_eq!(both.list(None).await.len(), 2);
    let mine = both.list(Some("chat")).await;
    assert_eq!(mine, [meta("ses_1", "chat", Some("Book club"))]);

    both.append("ses_1", &events("ses_1", 1..=3)).await.unwrap();
    both.append("ses_2", &events("ses_2", 1..=2)).await.unwrap();
    assert_eq!(both.purge(&["ses_1", "ses_9"]).await, 3);
    assert_eq!(both.read("ses_1", 0, 10).await, Err(StoreError::NotFound));
    assert_eq!(both.list(None).await.len(), 1);
    assert_eq!(both.read("ses_2", 0, 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn the_log_survives_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.db");
    let s = session("ses_1");
    {
        let store = SqliteSessionStore::open(&path).unwrap();
        store.append(&s, &events("ses_1", 1..=2)).await.unwrap();
        store.put_meta(&meta("ses_1", "chat", None)).await.unwrap();
    }
    let store = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(store.read(&s, 0, 10).await.unwrap(), events("ses_1", 1..=2));
    store.append(&s, &events("ses_1", 3..=3)).await.unwrap();
    assert_eq!(store.list_meta(None).await.unwrap().len(), 1);
}
