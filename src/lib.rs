mod api;
mod db;
pub mod model;
pub mod storage;

pub use api::router;
pub use db::{Database, Db, DbError, SeriesResult, WriteBatch, new_in_memory_database};
