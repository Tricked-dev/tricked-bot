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
        .batch_execute(include_str!("../migrations/003_profile_evolution.sql"))
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
    client
        .execute("DELETE FROM profile_candidate WHERE user_id = $1", &[&uid(user_id)])
        .await?;
    Ok(())
}

#[derive(serde::Serialize, Debug)]
pub struct ProfileCandidate {
    pub id: i64,
    pub user_id: i64,
    pub field: String,
    pub proposed_value: String,
    pub confirmations: i32,
}

pub async fn stage_profile_candidate(
    pool: &Pool,
    user_id: u64,
    field: &str,
    proposed_value: &str,
) -> Result<()> {
    let client = pool.get().await?;
    let row = client
        .query_one(
            r#"INSERT INTO profile_candidate (user_id, field, proposed_value, confirmations)
               VALUES ($1, $2, $3, 1)
               ON CONFLICT (user_id, field) DO UPDATE SET
                 proposed_value = EXCLUDED.proposed_value,
                 confirmations = CASE
                   WHEN profile_candidate.proposed_value = EXCLUDED.proposed_value
                   THEN profile_candidate.confirmations + 1 ELSE 1 END,
                 updated_at = now()
               RETURNING id, confirmations"#,
            &[&uid(user_id), &field, &proposed_value],
        )
        .await?;
    let candidate_id: i64 = row.get(0);
    let confirmations: i32 = row.get(1);
    if confirmations >= 3 {
        apply_profile_candidate_with_client(&client, candidate_id).await?;
    }
    Ok(())
}

async fn apply_profile_candidate_with_client(client: &tokio_postgres::Client, candidate_id: i64) -> Result<()> {
    let Some(row) = client
        .query_opt("SELECT user_id, field, proposed_value FROM profile_candidate WHERE id = $1", &[&candidate_id])
        .await?
    else {
        return Ok(());
    };
    let user_id: i64 = row.get(0);
    let field: String = row.get(1);
    let value: String = row.get(2);
    match field.as_str() {
        "relationship" => {
            client.execute("UPDATE \"user\" SET relationship = $1 WHERE id = $2", &[&value, &user_id]).await?;
        }
        "example" => {
            let pair: serde_json::Value = serde_json::from_str(&value)?;
            let input = pair.get("input").and_then(|v| v.as_str()).unwrap_or_default();
            let output = pair.get("output").and_then(|v| v.as_str()).unwrap_or_default();
            if !input.is_empty() && !output.is_empty() {
                client.execute(
                    "UPDATE \"user\" SET example_input = $1, example_output = $2 WHERE id = $3",
                    &[&input, &output, &user_id],
                ).await?;
            }
        }
        _ => return Err(color_eyre::eyre::eyre!("unsupported profile field: {}", field)),
    }
    client.execute("DELETE FROM profile_candidate WHERE id = $1", &[&candidate_id]).await?;
    Ok(())
}

pub async fn get_profile_candidates(pool: &Pool, user_id: u64) -> Result<Vec<ProfileCandidate>> {
    let client = pool.get().await?;
    let rows = client.query(
        "SELECT id, user_id, field, proposed_value, confirmations FROM profile_candidate WHERE user_id = $1 ORDER BY updated_at DESC",
        &[&uid(user_id)],
    ).await?;
    Ok(rows.into_iter().map(|row| ProfileCandidate {
        id: row.get(0), user_id: row.get(1), field: row.get(2), proposed_value: row.get(3), confirmations: row.get(4),
    }).collect())
}

pub async fn approve_profile_candidate(pool: &Pool, candidate_id: i64) -> Result<Option<i64>> {
    let client = pool.get().await?;
    let user_id = client.query_opt("SELECT user_id FROM profile_candidate WHERE id = $1", &[&candidate_id]).await?
        .map(|row| row.get::<_, i64>(0));
    apply_profile_candidate_with_client(&client, candidate_id).await?;
    Ok(user_id)
}

pub async fn reject_profile_candidate(pool: &Pool, candidate_id: i64) -> Result<Option<i64>> {
    let client = pool.get().await?;
    let rows = client.query("DELETE FROM profile_candidate WHERE id = $1 RETURNING user_id", &[&candidate_id]).await?;
    Ok(rows.first().map(|row| row.get(0)))
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
