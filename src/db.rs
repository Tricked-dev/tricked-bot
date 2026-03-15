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

pub async fn get_memories(pool: &Pool, user_id: u64) -> Result<Vec<Memory>> {
    let client = pool.get().await?;
    let rows = client
        .query(
            "SELECT * FROM memory WHERE user_id = $1 ORDER BY id DESC LIMIT 5",
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
