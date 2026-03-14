//! LanceDB schema, table management, and Arrow batch construction.

use anyhow::Result;
use arrow_array::{
    types::Float32Type, FixedSizeListArray, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use lancedb::connection::Connection;
use lancedb::index::{scalar::FtsIndexBuilder, Index};
use serde_json::json;
use std::sync::Arc;
use tracing::info;

/// Embedding dimension — BAAI/bge-small-en-v1.5 outputs 384 floats.
pub const EMBED_DIM: usize = 384;
/// Number of nearest neighbours to retrieve per index.
pub const TOP_K: usize = 5;
/// LanceDB directory — relative to project root (resolved at process CWD).
pub const DEFAULT_MEMORY_PATH: &str = "data/memory";
/// Unix socket path — relative to project root.
pub const DEFAULT_SOCKET_PATH: &str = "data/sockets/memory.sock";
/// FastEmbed model cache — relative to project root.
pub const DEFAULT_EMBED_CACHE: &str = "data/models";
/// Default logs directory — relative to project root.
pub const DEFAULT_LOGS_DIR: &str = "logs";
/// Long-term memory table (semantic recall across sessions).
pub const MEMORY_TABLE: &str = "memory";
/// Diary table — one stub row per completed ReAct turn; vectors back-filled at compaction.
pub const DIARY_TABLE: &str = "diary";
/// Knowledge table — curated facts and reference material.
pub const KNOWLEDGE_TABLE: &str = "knowledge";

/// Build the canonical Arrow schema for both memory tables.
pub fn table_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("metadata", DataType::Utf8, true),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBED_DIM as i32,
            ),
            false,
        ),
        Field::new("created_at", DataType::Utf8, false),
    ]))
}

/// Ensure `name` table exists in `db`.
///
/// If the table is absent it is created empty and immediately given a BM25
/// full-text search index on the `content` column.  This means every table
/// is born with hybrid (dense + keyword) search capability from the first
/// document insert — no retroactive index rebuild ever needed.
pub async fn ensure_table(db: &Connection, name: &str) -> Result<()> {
    let names = db.table_names().execute().await?;
    if !names.contains(&name.to_string()) {
        // Create the empty table with the canonical schema.
        db.create_empty_table(name.to_string(), table_schema())
            .execute()
            .await?;
        info!(table = name, "created memory table");

        // Immediately create a BM25 full-text search index on `content`.
        // Using the default `simple` tokenizer with English stemming/stopwords.
        let tbl = db.open_table(name).execute().await?;
        tbl.create_index(&["content"], Index::FTS(FtsIndexBuilder::default()))
            .execute()
            .await?;
        info!(table = name, "created BM25 FTS index on content column");
    }
    Ok(())
}

/// Construct a single-row [`RecordBatch`] ready for insertion.
pub fn make_batch(
    id: &str,
    content: &str,
    metadata: &str,
    vector: &[f32],
    created_at: &str,
) -> Result<RecordBatch> {
    let list_values: Vec<Option<f32>> = vector.iter().map(|&x| Some(x)).collect();
    let vec_arr = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        vec![Some(list_values)],
        EMBED_DIM as i32,
    );
    RecordBatch::try_new(
        table_schema(),
        vec![
            Arc::new(StringArray::from(vec![id])),
            Arc::new(StringArray::from(vec![content])),
            Arc::new(StringArray::from(vec![Some(metadata)])),
            Arc::new(vec_arr),
            Arc::new(StringArray::from(vec![created_at])),
        ],
    )
    .map_err(Into::into)
}

/// Create a [`RecordBatchIterator`] wrapping a single batch for insertion.
pub fn batch_stream(
    batch: RecordBatch,
) -> RecordBatchIterator<impl Iterator<Item = Result<RecordBatch, arrow_schema::ArrowError>>> {
    let schema = batch.schema();
    RecordBatchIterator::new(vec![batch].into_iter().map(Ok), schema)
}

/// Serialize an error message as a JSON object string: `{"error":"…"}`.
pub fn err_json(msg: &str) -> String {
    json!({ "error": msg }).to_string()
}
