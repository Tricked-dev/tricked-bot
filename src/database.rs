use serde::{Deserialize, Serialize};
use wb_sqlite::{CreateTableSql, InsertSync, UpdateSync};

use serde::Deserializer;
use std::fmt;

fn u64_from_str_or_int<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct U64Visitor;

    impl serde::de::Visitor<'_> for U64Visitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a u64 as an integer or a string")
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            value
                .parse::<u64>()
                .map_err(|_| E::custom(format!("invalid u64 string: {}", value)))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if value < 0 {
                return Err(E::custom("negative number cannot be cast to u64"));
            }
            Ok(value as u64)
        }
    }

    deserializer.deserialize_any(U64Visitor)
}

#[derive(CreateTableSql, InsertSync, UpdateSync, Serialize, Deserialize, Debug, PartialEq)]
pub struct User {
    #[sql(constraint = "PRIMARY KEY")]
    #[sql(typ = "INTEGER")]
    #[serde(deserialize_with = "u64_from_str_or_int")]
    pub id: u64,

    #[sql(constraint = "NOT NULL")]
    pub level: i32,

    #[sql(constraint = "NOT NULL")]
    pub name: String,

    #[sql(constraint = "NOT NULL")]
    pub xp: i32,
}

#[derive(CreateTableSql, InsertSync, UpdateSync, Serialize, Deserialize, Debug, PartialEq)]
#[sql(constraint = "UNIQUE(user_id,key)")]
pub struct Memory {
    #[sql(constraint = "PRIMARY KEY AUTOINCREMENT")]
    #[sql(typ = "INTEGER")]
    pub id: i32,
    #[sql(constraint = "NOT NULL")]
    pub user_id: String,
    #[sql(constraint = "NOT NULL")]
    pub content: String,
    #[sql(constraint = "NOT NULL")]
    pub key: String,
}
