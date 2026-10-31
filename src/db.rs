use color_eyre::Result;
use deadpool_postgres::Pool;
use postgres_from_row::FromRow;
use serde::Serialize;

use crate::levels::xp_required_for_level;

/// Every migration is idempotent and re-applied on startup, in order.
const MIGRATIONS: [&str; 4] = [
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_improve_types.sql"),
    include_str!("../migrations/004_memory_archive.sql"),
    include_str!("../migrations/005_drop_profile_evolution.sql"),
];

const USER_COLUMNS: &str = r#"id, level, xp, name, relationship, example_input, example_output"#;
const MEMORY_COLUMNS: &str = "id, user_id, key, content";

#[derive(FromRow, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub level: i32,
    pub xp: i32,
    pub name: String,
    pub relationship: String,
    pub example_input: String,
    pub example_output: String,
}

impl User {
    /// The stored name, or a placeholder for users whose name was never recorded.
    pub fn display_name(&self) -> String {
        if self.name.is_empty() { format!("User {}", self.id) } else { self.name.clone() }
    }
}

#[derive(FromRow, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Memory {
    pub id: i64,
    pub user_id: i64,
    pub key: String,
    pub content: String,
}

/// Discord snowflakes fit in 63 bits, so the cast is lossless.
const fn sql_id(id: u64) -> i64 {
    id as i64
}

pub async fn run_migrations(pool: &Pool) -> Result<()> {
    let client = pool.get().await?;
    for migration in MIGRATIONS {
        client.batch_execute(migration).await?;
    }
    Ok(())
}

/// Both DM authors and users intercepted by responders must have a stable DB identity.
pub async fn ensure_user(pool: &Pool, id: u64, name: &str) -> Result<()> {
    pool.get()
        .await?
        .execute(
            r#"INSERT INTO "user" (id, name, level, xp, social_credit, relationship, example_input, example_output)
               VALUES ($1, $2, 0, 0, 0, '', '', '') ON CONFLICT (id) DO NOTHING"#,
            &[&sql_id(id), &name],
        )
        .await?;
    Ok(())
}

/// Adds XP to a user, creating them on their first message without any XP.
///
/// Returns the new level when the gain reached the next threshold. `carry_over` keeps the surplus XP for the new
/// level instead of starting it at zero. Both statements are atomic, so concurrent messages can't lose XP or level up
/// twice.
pub async fn award_xp(pool: &Pool, id: u64, name: &str, gained: i32, carry_over: bool) -> Result<Option<i32>> {
    let client = pool.get().await?;
    let award = client
        .prepare_cached(
            r#"INSERT INTO "user" (id, name, level, xp, social_credit, relationship, example_input, example_output)
               VALUES ($1, $2, 0, 0, 0, '', '', '')
               ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, xp = "user".xp + $3
               RETURNING level, xp, (xmax = 0) AS inserted"#,
        )
        .await?;
    let row = client.query_one(&award, &[&sql_id(id), &name, &gained]).await?;
    let (level, xp, inserted): (i32, i32, bool) = (row.get(0), row.get(1), row.get(2));
    let required = xp_required_for_level(level);
    if inserted || xp < required {
        return Ok(None);
    }
    let level_up =
        client.prepare_cached(r#"UPDATE "user" SET level = level + 1, xp = $3 WHERE id = $1 AND level = $2"#).await?;
    let remaining = if carry_over { xp - required } else { 0 };
    let updated = client.execute(&level_up, &[&sql_id(id), &level, &remaining]).await?;
    Ok((updated == 1).then_some(level + 1))
}

pub async fn user(pool: &Pool, id: u64) -> Result<Option<User>> {
    let client = pool.get().await?;
    let statement = client.prepare_cached(&format!(r#"SELECT {USER_COLUMNS} FROM "user" WHERE id = $1"#)).await?;
    Ok(client.query_opt(&statement, &[&sql_id(id)]).await?.as_ref().map(User::from_row))
}

pub async fn users(pool: &Pool) -> Result<Vec<User>> {
    let rows = pool.get().await?.query(&format!(r#"SELECT {USER_COLUMNS} FROM "user" ORDER BY id"#), &[]).await?;
    Ok(rows.iter().map(User::from_row).collect())
}

/// One-based leaderboard position of someone at `level`; ties share the better rank.
pub async fn rank(pool: &Pool, level: i32) -> Result<i64> {
    let row = pool.get().await?.query_one(r#"SELECT count(*) + 1 FROM "user" WHERE level > $1"#, &[&level]).await?;
    Ok(row.get(0))
}

pub async fn update_profile(pool: &Pool, user: &User) -> Result<()> {
    pool.get()
        .await?
        .execute(
            r#"UPDATE "user" SET name = $1, relationship = $2, example_input = $3, example_output = $4 WHERE id = $5"#,
            &[&user.name, &user.relationship, &user.example_input, &user.example_output, &user.id],
        )
        .await?;
    Ok(())
}

pub async fn memories(pool: &Pool, user_id: u64) -> Result<Vec<Memory>> {
    let client = pool.get().await?;
    let statement =
        client.prepare_cached(&format!("SELECT {MEMORY_COLUMNS} FROM memory WHERE user_id = $1 ORDER BY id")).await?;
    let rows = client.query(&statement, &[&sql_id(user_id)]).await?;
    Ok(rows.iter().map(Memory::from_row).collect())
}

pub async fn all_memories(pool: &Pool) -> Result<Vec<Memory>> {
    let rows =
        pool.get().await?.query(&format!("SELECT {MEMORY_COLUMNS} FROM memory ORDER BY user_id, id"), &[]).await?;
    Ok(rows.iter().map(Memory::from_row).collect())
}

pub async fn memory(pool: &Pool, id: i64) -> Result<Option<Memory>> {
    let row =
        pool.get().await?.query_opt(&format!("SELECT {MEMORY_COLUMNS} FROM memory WHERE id = $1"), &[&id]).await?;
    Ok(row.as_ref().map(Memory::from_row))
}

pub async fn create_memory(pool: &Pool, user_id: u64, key: &str, content: &str) -> Result<()> {
    pool.get()
        .await?
        .execute("INSERT INTO memory (user_id, key, content) VALUES ($1, $2, $3)", &[&sql_id(user_id), &key, &content])
        .await?;
    Ok(())
}

pub async fn upsert_memory(pool: &Pool, user_id: u64, key: &str, content: &str) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "INSERT INTO memory (user_id, key, content) VALUES ($1, $2, $3)
             ON CONFLICT (user_id, key) DO UPDATE SET content = EXCLUDED.content",
            &[&sql_id(user_id), &key, &content],
        )
        .await?;
    Ok(())
}

/// Returns the owner of the updated memory, or `None` when it doesn't exist.
pub async fn update_memory(pool: &Pool, id: i64, key: &str, content: &str) -> Result<Option<i64>> {
    let row = pool
        .get()
        .await?
        .query_opt("UPDATE memory SET key = $1, content = $2 WHERE id = $3 RETURNING user_id", &[&key, &content, &id])
        .await?;
    Ok(row.map(|row| row.get(0)))
}

/// Returns the owner of the deleted memory, or `None` when it doesn't exist.
pub async fn delete_memory(pool: &Pool, id: i64) -> Result<Option<i64>> {
    let row = pool.get().await?.query_opt("DELETE FROM memory WHERE id = $1 RETURNING user_id", &[&id]).await?;
    Ok(row.map(|row| row.get(0)))
}

pub async fn users_over_memory_limit(pool: &Pool, limit: usize) -> Result<Vec<u64>> {
    let rows = pool
        .get()
        .await?
        .query("SELECT user_id FROM memory GROUP BY user_id HAVING count(*) > $1", &[&(limit as i64)])
        .await?;
    Ok(rows.iter().map(|row| row.get::<_, i64>(0) as u64).collect())
}

/// Archive only the exact snapshot Jev reviewed, and only while still over the cap.
pub async fn archive_overflow_memory(pool: &Pool, memory: &Memory, limit: usize) -> Result<bool> {
    let mut client = pool.get().await?;
    let tx = client.transaction().await?;
    // Held only for this short transaction, never during inference. Also coordinates web edits.
    tx.batch_execute("LOCK TABLE memory IN SHARE ROW EXCLUSIVE MODE").await?;
    let count: i64 = tx.query_one("SELECT count(*) FROM memory WHERE user_id = $1", &[&memory.user_id]).await?.get(0);
    if count <= limit as i64 {
        return Ok(false);
    }
    let params: [&(dyn tokio_postgres::types::ToSql + Sync); 4] =
        [&memory.id, &memory.user_id, &memory.key, &memory.content];
    let deleted =
        tx.execute("DELETE FROM memory WHERE id = $1 AND user_id = $2 AND key = $3 AND content = $4", &params).await?;
    if deleted == 0 {
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO memory_archive (memory_id, user_id, key, content, reason)
         VALUES ($1, $2, $3, $4, 'Jev overflow cleanup')",
        &params,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Stores a newly generated quiz question. Returns `false` when it was asked before.
pub async fn remember_math_question(pool: &Pool, question: &str, answer: f64) -> Result<bool> {
    let inserted = pool
        .get()
        .await?
        .execute(
            "INSERT INTO math_question (question, answer)
             SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM math_question WHERE question = $1)",
            &[&question, &answer],
        )
        .await?;
    Ok(inserted == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool whose connections live in a fresh schema of the `TEST_DATABASE_URL` database.
    async fn scratch_pool() -> (Pool, String) {
        let url = std::env::var("TEST_DATABASE_URL").expect("test database");
        let schema = format!("tricked_bot_test_{}", std::process::id());
        let mut config: tokio_postgres::Config = url.parse().unwrap();
        config.options(format!("-c search_path={schema}"));
        let pool = Pool::builder(deadpool_postgres::Manager::new(config, tokio_postgres::NoTls)).build().unwrap();
        pool.get().await.unwrap().batch_execute(&format!("CREATE SCHEMA {schema}")).await.unwrap();
        (pool, schema)
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL; creates and drops its own schema"]
    async fn queries_work_against_the_migrated_schema() {
        let (pool, schema) = scratch_pool().await;
        run_migrations(&pool).await.unwrap();
        run_migrations(&pool).await.unwrap();

        assert_eq!(award_xp(&pool, 1, "alice", 50, false).await.unwrap(), None, "first message only creates");
        assert_eq!(award_xp(&pool, 1, "alice", 10, false).await.unwrap(), Some(1), "level 0 needs no XP");
        assert_eq!(award_xp(&pool, 1, "alice", 25, true).await.unwrap(), Some(2));
        let alice = user(&pool, 1).await.unwrap().unwrap();
        assert_eq!((alice.level, alice.xp), (2, 5), "surplus over the 20 XP needed for level 2 carries over");

        ensure_user(&pool, 2, "bob").await.unwrap();
        let bob = User { relationship: "nemesis".into(), ..user(&pool, 2).await.unwrap().unwrap() };
        update_profile(&pool, &bob).await.unwrap();
        assert_eq!(user(&pool, 2).await.unwrap().unwrap().relationship, "nemesis");
        award_xp(&pool, 2, "bob", 1, false).await.unwrap();
        let racing = (0..8).map(|_| award_xp(&pool, 2, "bob", 1_000, false));
        let mut level_ups: Vec<_> =
            futures_util::future::join_all(racing).await.into_iter().filter_map(Result::unwrap).collect();
        level_ups.sort_unstable();
        let bob = user(&pool, 2).await.unwrap().unwrap();
        assert!(!level_ups.is_empty());
        assert_eq!(level_ups, (2..=bob.level).collect::<Vec<_>>(), "racing awards never skip or repeat a level");
        assert_eq!((rank(&pool, bob.level).await.unwrap(), rank(&pool, 0).await.unwrap()), (1, 3));
        assert_eq!(users(&pool).await.unwrap().len(), 2);

        upsert_memory(&pool, 1, "work", "builds bots").await.unwrap();
        upsert_memory(&pool, 1, "work", "builds better bots").await.unwrap();
        create_memory(&pool, 1, "food", "hates cilantro").await.unwrap();
        let memories = memories(&pool, 1).await.unwrap();
        assert_eq!(
            memories.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
            ["builds better bots", "hates cilantro"]
        );
        assert_eq!(update_memory(&pool, memories[1].id, "food", "loves cilantro").await.unwrap(), Some(1));
        assert_eq!(memory(&pool, memories[1].id).await.unwrap().unwrap().content, "loves cilantro");
        assert_eq!(users_over_memory_limit(&pool, 1).await.unwrap(), [1]);
        assert_eq!(delete_memory(&pool, memories[1].id).await.unwrap(), Some(1));
        assert_eq!(delete_memory(&pool, memories[1].id).await.unwrap(), None);
        assert_eq!(all_memories(&pool).await.unwrap().len(), 1);

        assert!(remember_math_question(&pool, "1 + 1", 2.0).await.unwrap());
        assert!(!remember_math_question(&pool, "1 + 1", 2.0).await.unwrap());

        pool.get().await.unwrap().batch_execute(&format!("DROP SCHEMA {schema} CASCADE")).await.unwrap();
    }
}

#[cfg(test)]
mod memory_archive_tests {
    use super::*;
    use crate::ai::{cleanup, memory::AuthorLocks, recall::LIMIT};

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL; uses session-local temporary tables only"]
    async fn overflow_archive_is_recoverable_and_preserves_edits_and_cap() {
        let config = std::env::var("TEST_DATABASE_URL").expect("test database").parse().unwrap();
        let manager = deadpool_postgres::Manager::new(config, tokio_postgres::NoTls);
        let pool = Pool::builder(manager).max_size(1).build().unwrap();
        pool.get()
            .await
            .unwrap()
            .batch_execute(
                "CREATE TEMP TABLE memory (id BIGINT PRIMARY KEY, user_id BIGINT, key TEXT, content TEXT);
                 CREATE TEMP TABLE memory_archive (memory_id BIGINT, user_id BIGINT, key TEXT, content TEXT, reason TEXT);
                 INSERT INTO memory SELECT i, 1, 'key' || i, 'fact' FROM generate_series(1, 256) i;",
            )
            .await
            .unwrap();
        let memory = Memory { id: 1, user_id: 1, key: "key1".into(), content: "fact".into() };
        let stale = Memory { content: "old content".into(), ..memory.clone() };
        assert!(!archive_overflow_memory(&pool, &stale, LIMIT).await.unwrap());
        let other = Memory { user_id: 2, ..memory.clone() };
        assert!(!archive_overflow_memory(&pool, &other, LIMIT).await.unwrap());

        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|axum::Json(body): axum::Json<serde_json::Value>| async move {
                let options = body["questions"]["selection"]["criteria"].as_object().unwrap();
                assert_eq!(options.len(), 32);
                assert!(!options.contains_key("m256"));
                let probabilities: serde_json::Map<_, _> = options
                    .keys()
                    .map(|id| (id.clone(), serde_json::json!(if id == "m1" { 1.0 } else { 0.0 })))
                    .collect();
                axum::Json(serde_json::json!({"answers": {"selection": {"probabilities": probabilities}}}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let decider = crate::decider::Decider::new(reqwest::Client::new(), url, "test".into()).unwrap();
        cleanup::clean_user(&pool, &decider, &AuthorLocks::default(), 1).await.unwrap();
        server.abort();

        let next = Memory { id: 2, user_id: 1, key: "key2".into(), content: "fact".into() };
        assert!(!archive_overflow_memory(&pool, &next, LIMIT).await.unwrap());
        let client = pool.get().await.unwrap();
        let count: i64 = client.query_one("SELECT count(*) FROM memory", &[]).await.unwrap().get(0);
        assert_eq!(count, 255);
        let content: String =
            client.query_one("SELECT content FROM memory_archive WHERE memory_id = 1", &[]).await.unwrap().get(0);
        assert_eq!(content, "fact");
    }
}
