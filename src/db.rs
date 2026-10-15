use color_eyre::Result;
use deadpool_postgres::Pool;
use postgres_from_row::FromRow;

use crate::database::{Memory, MathQuestion, User};

fn uid(id: u64) -> i64 {
    id as i64
}

/// Run schema migrations in order (idempotent).
pub async fn run_migrations(pool: &Pool) -> Result<()> {
    let client = pool.get().await?;
    client
        .batch_execute(include_str!("../migrations/001_init.sql"))
        .await?;
    client
        .batch_execute(include_str!("../migrations/002_improve_types.sql"))
        .await?;
    client
        .batch_execute(include_str!("../migrations/005_drop_profile_evolution.sql"))
        .await?;
    client.batch_execute(include_str!("../migrations/004_memory_archive.sql")).await?;
    Ok(())
}

pub async fn get_user(pool: &Pool, id: u64) -> Result<Option<User>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM \"user\" WHERE id = $1", &[&uid(id)])
        .await?;
    Ok(rows.first().map(User::from_row))
}

pub async fn insert_user(pool: &Pool, user: &User) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "INSERT INTO \"user\" (id, level, xp, social_credit, name, relationship, example_input, example_output)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (id) DO NOTHING",
        &[&user.id, &user.level, &user.xp, &user.social_credit,
          &user.name, &user.relationship, &user.example_input, &user.example_output],
    ).await?;
    Ok(())
}

pub async fn update_user_xp(pool: &Pool, user: &User) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "UPDATE \"user\" SET level = $1, xp = $2, name = $3 WHERE id = $4",
        &[&user.level, &user.xp, &user.name, &user.id],
    ).await?;
    Ok(())
}

pub async fn get_user_by_name(pool: &Pool, name: &str) -> Result<Option<User>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM \"user\" WHERE lower(name) = lower($1)", &[&name])
        .await?;
    Ok(rows.first().map(User::from_row))
}

pub async fn get_all_users_ranked(pool: &Pool) -> Result<Vec<User>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM \"user\" WHERE level != 0 ORDER BY level DESC", &[])
        .await?;
    Ok(rows.iter().map(User::from_row).collect())
}

pub async fn update_user_profile(
    pool: &Pool,
    user_id: u64,
    name: &str,
    relationship: &str,
    example_input: &str,
    example_output: &str,
) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "UPDATE \"user\" SET name = $1, relationship = $2, example_input = $3, example_output = $4 WHERE id = $5",
        &[&name, &relationship, &example_input, &example_output, &uid(user_id)],
    ).await?;
    Ok(())
}

pub async fn get_all_memories(pool: &Pool, user_id: u64) -> Result<Vec<Memory>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT * FROM memory WHERE user_id = $1 ORDER BY key",
            &[&uid(user_id)],
        )
        .await?;
    Ok(rows.iter().map(Memory::from_row).collect())
}

pub async fn upsert_memory(pool: &Pool, user_id: u64, key: &str, content: &str) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "INSERT INTO memory (user_id, key, content) VALUES ($1, $2, $3)
         ON CONFLICT (user_id, key) DO UPDATE SET content = EXCLUDED.content",
        &[&uid(user_id), &key, &content],
    ).await?;
    Ok(())
}

pub async fn get_memory(pool: &Pool, memory_id: i64) -> Result<Option<Memory>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM memory WHERE id = $1", &[&memory_id])
        .await?;
    Ok(rows.first().map(Memory::from_row))
}

pub async fn create_memory(pool: &Pool, user_id: u64, key: &str, content: &str) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "INSERT INTO memory (user_id, key, content) VALUES ($1, $2, $3)",
        &[&uid(user_id), &key, &content],
    ).await?;
    Ok(())
}

pub async fn update_memory(pool: &Pool, memory_id: i64, key: &str, content: &str) -> Result<()> {
    let client = pool.get().await?;
    client.execute(
        "UPDATE memory SET key = $1, content = $2 WHERE id = $3",
        &[&key, &content, &memory_id],
    ).await?;
    Ok(())
}

pub async fn delete_memory(pool: &Pool, memory_id: i64) -> Result<Option<i64>> {
    let client = pool.get().await?;
    let rows = client
        .query("DELETE FROM memory WHERE id = $1 RETURNING user_id", &[&memory_id])
        .await?;
    Ok(rows.first().map(|r| r.get::<_, i64>(0)))
}

pub async fn get_memories_for_user_web(pool: &Pool, user_id: u64) -> Result<Vec<Memory>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM memory WHERE user_id = $1 ORDER BY id", &[&uid(user_id)])
        .await?;
    Ok(rows.iter().map(Memory::from_row).collect())
}

pub async fn get_users_with_relationships(pool: &Pool) -> Result<Vec<(String, String)>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT name, relationship FROM \"user\" WHERE relationship != ''",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1))).collect())
}

pub async fn get_users_with_examples(pool: &Pool) -> Result<Vec<(String, String, String)>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT name, example_input, example_output FROM \"user\" WHERE example_input != '' AND example_output != ''",
            &[],
        )
        .await?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
}

pub async fn get_all_users_web(pool: &Pool) -> Result<Vec<User>> {
    let client = pool.get().await?;
    let rows = client
        .query("SELECT * FROM \"user\" ORDER BY id", &[])
        .await?;
    Ok(rows.iter().map(User::from_row).collect())
}

pub async fn get_math_questions(pool: &Pool) -> Result<Vec<MathQuestion>> {
    let client = pool.get().await?;
    let rows = client.query("SELECT * FROM math_question", &[]).await?;
    Ok(rows.iter().map(MathQuestion::from_row).collect())
}

/// Archive only the exact snapshot Jev reviewed, and only while still over the cap.
pub async fn archive_overflow_memory(pool: &Pool, memory: &Memory) -> Result<bool> {
    let mut client = pool.get().await?;
    let tx = client.transaction().await?;
    // Held only for this short transaction, never during inference. Also coordinates web edits.
    tx.batch_execute("LOCK TABLE memory IN SHARE ROW EXCLUSIVE MODE").await?;
    let count: i64 = tx.query_one("SELECT count(*) FROM memory WHERE user_id=$1", &[&memory.user_id]).await?.get(0);
    if count <= crate::recall::LIMIT as i64 { return Ok(false); }
    let row = tx.query_opt("DELETE FROM memory WHERE id=$1 AND user_id=$2 AND key=$3 AND content=$4 RETURNING id",
        &[&memory.id,&memory.user_id,&memory.key,&memory.content]).await?;
    if row.is_none() { return Ok(false); }
    tx.execute("INSERT INTO memory_archive(memory_id,user_id,key,content,reason) VALUES($1,$2,$3,$4,'Jev overflow cleanup')",
        &[&memory.id,&memory.user_id,&memory.key,&memory.content]).await?;
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod memory_archive_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL; uses session-local temporary tables only"]
    async fn overflow_archive_is_recoverable_and_preserves_edits_and_cap() {
        let config = std::env::var("TEST_DATABASE_URL").expect("test database").parse().unwrap();
        let manager = deadpool_postgres::Manager::new(config,tokio_postgres::NoTls);
        let pool=Pool::builder(manager).max_size(1).build().unwrap();
        let client=pool.get().await.unwrap();
        client.batch_execute("CREATE TEMP TABLE memory (id BIGINT PRIMARY KEY,user_id BIGINT,key TEXT,content TEXT); CREATE TEMP TABLE memory_archive (memory_id BIGINT,user_id BIGINT,key TEXT,content TEXT,reason TEXT); INSERT INTO memory SELECT i,1,'key'||i,'fact' FROM generate_series(1,256) i;").await.unwrap();
        drop(client);
        let m=Memory{id:1,user_id:1,key:"key1".into(),content:"fact".into()};
        let mut stale=m.clone();stale.content="old content".into();
        assert!(!archive_overflow_memory(&pool,&stale).await.unwrap());
        let mut other=m.clone();other.user_id=2;
        assert!(!archive_overflow_memory(&pool,&other).await.unwrap());
        let app=axum::Router::new().route("/",axum::routing::post(|axum::Json(body):axum::Json<serde_json::Value>|async move {
            let options=body["questions"]["selection"]["criteria"].as_object().unwrap();
            assert_eq!(options.len(),32);
            assert!(!options.contains_key("m256"));
            let probabilities:serde_json::Map<String,serde_json::Value>=options.keys()
                .map(|id|(id.clone(),serde_json::json!(if id=="m1" {1.0} else {0.0}))).collect();
            axum::Json(serde_json::json!({"answers":{"selection":{"probabilities":probabilities}}}))
        }));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
        let decider=crate::decider::Decider::new(url,"test".into()).unwrap();
        crate::memory_cleanup::clean_user(&pool,&decider,1).await.unwrap();
        server.abort();
        let next=Memory{id:2,user_id:1,key:"key2".into(),content:"fact".into()};
        assert!(!archive_overflow_memory(&pool,&next).await.unwrap());
        let client=pool.get().await.unwrap();
        let count:i64=client.query_one("SELECT count(*) FROM memory",&[]).await.unwrap().get(0);assert_eq!(count,255);
        let content:String=client.query_one("SELECT content FROM memory_archive WHERE memory_id=1",&[]).await.unwrap().get(0);assert_eq!(content,"fact");
    }
}
