use postgres_from_row::FromRow;
use serde::{Deserialize, Serialize};

#[derive(FromRow, Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct User {
    pub id: i64,
    pub level: i32,
    pub xp: i32,
    pub social_credit: i64,
    pub name: String,
    pub relationship: String,
    pub example_input: String,
    pub example_output: String,
}

impl User {
    pub fn discord_id(&self) -> u64 {
        self.id as u64
    }
}

#[derive(FromRow, Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct Memory {
    pub id: i64,
    pub user_id: i64,
    pub content: String,
    pub key: String,
}

#[derive(FromRow, Serialize, Deserialize, Debug, PartialEq, Clone)]
pub struct MathQuestion {
    pub id: i64,
    pub question: String,
    pub answer: f64,
}
