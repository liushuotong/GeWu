//! Experience store implementation combining SQLite and Qdrant.

use crate::config::Config;
use crate::types::{Experience, ExperienceQuery, RecalledExperience};
use anyhow::{Context, Result};
use embed_anything::embeddings::embed::{Embedder, EmbeddingResult};
use qdrant_client::qdrant::{
    Distance, NamedVectors, PointStruct, Query, QueryPointsBuilder, VectorParamsBuilder, VectorsConfig,
};
use qdrant_client::Qdrant;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

pub struct ExperienceStore {
    pool: sqlx::SqlitePool,
    qdrant: Arc<Qdrant>,
    embedding_model: RwLock<Embedder>,
    config: Config,
}

impl ExperienceStore {
    pub async fn new(config: Config) -> Result<Self> {
        let db_url = if config.db_path.is_absolute() {
            format!("sqlite://{}?mode=rwc", config.db_path.display())
        } else {
            format!("sqlite:{}?mode=rwc", config.db_path.display())
        };

        if let Some(parent) = config.db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).context("Failed to create database directory")?;
            }
        }

        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .context("Failed to connect to SQLite")?;

        let store = Self {
            pool,
            qdrant: Self::create_qdrant_client(&config)?,
            embedding_model: RwLock::new(Embedder::from_pretrained_hf(
                &config.embedding_model,
                None,
                None,
                None,
            )?),
            config,
        };

        store.init_schema().await?;
        store.init_qdrant_collection().await?;

        Ok(store)
    }

    fn create_qdrant_client(config: &Config) -> Result<Arc<Qdrant>> {
        let mut builder = Qdrant::from_url(&config.qdrant_url);
        if let Some(ref api_key) = config.qdrant_api_key {
            builder = builder.api_key(api_key.as_str());
        }
        let client = builder.build()?;
        Ok(Arc::new(client))
    }

    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS experiences (
                id TEXT PRIMARY KEY,
                task_description TEXT NOT NULL,
                task_type TEXT NOT NULL,
                problem_summary TEXT NOT NULL,
                solution_summary TEXT NOT NULL,
                tools_used TEXT NOT NULL,
                iteration_count INTEGER NOT NULL,
                success INTEGER NOT NULL,
                embedding_id TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .context("Failed to create experiences table")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_experiences_task_type ON experiences(task_type)"
        )
        .execute(&self.pool)
        .await
        .context("Failed to create task_type index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_experiences_success ON experiences(success)"
        )
        .execute(&self.pool)
        .await
        .context("Failed to create success index")?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_experiences_created_at ON experiences(created_at)"
        )
        .execute(&self.pool)
        .await
        .context("Failed to create created_at index")?;

        info!("Experience database schema initialized");
        Ok(())
    }

    async fn init_qdrant_collection(&self) -> Result<()> {
        let collection_name = &self.config.collection_name;

        let exists = self.qdrant.collection_exists(collection_name).await?;

        if !exists {
            let vectors_config = VectorsConfig {
                config: Some(
                    qdrant_client::qdrant::vectors_config::Config::from(
                        qdrant_client::qdrant::VectorParamsMap {
                            map: HashMap::from([(
                                "dense".to_string(),
                                VectorParamsBuilder::new(
                                    self.config.embedding_dimension as u64,
                                    Distance::Cosine,
                                )
                                .build(),
                            )]),
                        },
                    ),
                ),
            };

            let create = qdrant_client::qdrant::CreateCollectionBuilder::new(collection_name)
                .vectors_config(vectors_config);

            self.qdrant.create_collection(create).await?;
            info!("Created Qdrant collection: {}", collection_name);
        }

        Ok(())
    }

    pub async fn store(&self, experience: &Experience) -> Result<()> {
        let tools_json = serde_json::to_string(&experience.tools_used)
            .context("Failed to serialize tools_used")?;

        sqlx::query(
            r#"
            INSERT INTO experiences
            (id, task_description, task_type, problem_summary, solution_summary,
             tools_used, iteration_count, success, embedding_id, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&experience.id)
        .bind(&experience.task_description)
        .bind(&experience.task_type)
        .bind(&experience.problem_summary)
        .bind(&experience.solution_summary)
        .bind(&tools_json)
        .bind(experience.iteration_count)
        .bind(experience.success as i32)
        .bind(&experience.embedding_id)
        .bind(experience.created_at.timestamp())
        .execute(&self.pool)
        .await
        .context("Failed to insert experience into SQLite")?;

        let search_text = experience.search_text();
        let embedding = self.compute_embedding(&search_text).await?;

        let mut payload = qdrant_client::Payload::new();
        payload.insert(
            "experience_id".to_string(),
            qdrant_client::qdrant::Value::from(experience.id.clone()),
        );
        payload.insert(
            "task_type".to_string(),
            qdrant_client::qdrant::Value::from(experience.task_type.clone()),
        );

        let point_id = experience
            .embedding_id
            .parse::<u64>()
            .unwrap_or_else(|_| {
                experience
                    .id
                    .split('-')
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
            });

        let named_vectors = NamedVectors::default()
            .add_vector("dense", embedding);

        let point = PointStruct::new(point_id, named_vectors, payload);

        let collection_name = &self.config.collection_name;
        self.qdrant
            .upsert_points(
                qdrant_client::qdrant::UpsertPointsBuilder::new(collection_name, vec![point])
                    .wait(true),
            )
            .await
            .context("Failed to upsert embedding to Qdrant")?;

        info!(
            "Stored experience: {} ({})",
            experience.id, experience.task_type
        );
        Ok(())
    }

    pub async fn recall(&self, query: &ExperienceQuery) -> Result<Vec<RecalledExperience>> {
        let embedding = self.compute_embedding(&query.query_text).await?;
        let collection_name = &self.config.collection_name;

        let request = QueryPointsBuilder::new(collection_name)
            .limit(query.limit as u64)
            .query(Query::new_nearest(embedding))
            .using("dense")
            .score_threshold(query.score_threshold)
            .with_payload(true);

        let search_result = self.qdrant.query(request).await?;

        let mut recalled = Vec::new();
        for point in search_result.result {
            let experience_id = point
                .payload
                .get("experience_id")
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default();

            if !experience_id.is_empty() {
                if let Some(exp) = self.get_by_id(&experience_id).await? {
                    recalled.push(RecalledExperience {
                        experience: exp,
                        similarity_score: point.score,
                    });
                }
            }
        }

        debug!(
            "Recalled {} experiences for query: {}",
            recalled.len(),
            query.query_text
        );
        Ok(recalled)
    }

    pub async fn search_by_task_type(
        &self,
        task_type: &str,
        limit: usize,
    ) -> Result<Vec<Experience>> {
        let rows = sqlx::query(
            r#"
            SELECT * FROM experiences
            WHERE task_type = ? AND success = 1
            ORDER BY created_at DESC
            LIMIT ?
            "#,
        )
        .bind(task_type)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("Failed to query experiences by task type")?;

        let mut experiences = Vec::new();
        for row in rows {
            if let Ok(exp) = self.row_to_experience(&row) {
                experiences.push(exp);
            }
        }

        Ok(experiences)
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Option<Experience>> {
        let row = sqlx::query("SELECT * FROM experiences WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("Failed to query experience by ID")?;

        match row {
            Some(r) => Ok(Some(self.row_to_experience(&r)?)),
            None => Ok(None),
        }
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        if let Some(_exp) = self.get_by_id(id).await? {
            sqlx::query("DELETE FROM experiences WHERE id = ?")
                .bind(id)
                .execute(&self.pool)
                .await
                .context("Failed to delete experience from SQLite")?;
            info!("Deleted experience from SQLite: {}", id);
        }
        Ok(())
    }

    pub async fn list_ids(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT id FROM experiences ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
            .context("Failed to list experience IDs")?;

        let mut ids = Vec::new();
        for row in rows {
            if let Ok(id) = row.try_get::<String, _>("id") {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    pub async fn count(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COUNT(*) as count FROM experiences")
            .fetch_one(&self.pool)
            .await
            .context("Failed to count experiences")?;

        Ok(row.try_get::<i64, _>("count")?)
    }

    pub async fn list_all(&self) -> Result<Vec<Experience>> {
        let rows = sqlx::query(
            r#"SELECT id, task_description, task_type, problem_summary, solution_summary,
                      tools_used, iteration_count, success, embedding_id, created_at
               FROM experiences ORDER BY created_at DESC"#
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to list experiences")?;

        let mut experiences = Vec::new();
        for row in rows {
            let tools_json = row.try_get::<String, _>("tools_used")?;
            let tools_used: Vec<String> = serde_json::from_str(&tools_json).unwrap_or_default();

            experiences.push(Experience {
                id: row.try_get("id")?,
                task_description: row.try_get("task_description")?,
                task_type: row.try_get("task_type")?,
                problem_summary: row.try_get("problem_summary")?,
                solution_summary: row.try_get("solution_summary")?,
                tools_used,
                iteration_count: row.try_get("iteration_count")?,
                success: row.try_get::<i32, _>("success")? != 0,
                embedding_id: row.try_get("embedding_id")?,
                created_at: chrono::DateTime::from_timestamp(row.try_get::<i64, _>("created_at")?, 0)
                    .unwrap_or_else(chrono::Utc::now),
            });
        }
        Ok(experiences)
    }

    async fn compute_embedding(&self, text: &str) -> Result<Vec<f32>> {
        let model = self.embedding_model.read().await;
        let query_text = "query: ".to_string() + text;
        let results = model.embed(&[query_text.as_str()], None, None).await?;

        match results.first() {
            Some(EmbeddingResult::DenseVector(v)) => Ok(v.clone()),
            Some(EmbeddingResult::MultiVector(vecs)) => {
                Ok(vecs.iter().flatten().cloned().collect())
            }
            None => Err(anyhow::anyhow!("No embedding returned")),
        }
    }

    fn row_to_experience(&self, row: &SqliteRow) -> Result<Experience> {
        let tools_json: String = row.try_get("tools_used")?;
        let tools_used: Vec<String> = serde_json::from_str(&tools_json)
            .context("Failed to deserialize tools_used")?;

        let created_at: i64 = row.try_get("created_at")?;
        let created_at = chrono::DateTime::from_timestamp(created_at, 0)
            .unwrap_or_else(|| chrono::Utc::now());

        let id: String = row.try_get("id")?;
        let task_description: String = row.try_get("task_description")?;
        let task_type: String = row.try_get("task_type")?;
        let problem_summary: String = row.try_get("problem_summary")?;
        let solution_summary: String = row.try_get("solution_summary")?;
        let iteration_count: i32 = row.try_get("iteration_count")?;
        let success: i32 = row.try_get("success")?;
        let embedding_id: String = row.try_get("embedding_id")?;

        Ok(Experience {
            id,
            task_description,
            task_type,
            problem_summary,
            solution_summary,
            tools_used,
            iteration_count,
            success: success != 0,
            embedding_id,
            created_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_test_config(tmp_dir: &std::path::Path) -> Config {
        Config {
            qdrant_url: "http://localhost:6334".to_string(),
            qdrant_api_key: None,
            collection_name: "test_experiences".to_string(),
            db_path: tmp_dir.join("test_experiences.db"),
            embedding_model: "BAAI/bge-base-en-v1.5".to_string(),
            default_score_threshold: 0.7,
            embedding_dimension: 768,
        }
    }

    #[tokio::test]
    async fn test_store_and_retrieve() {
        let tmp_dir = tempdir().unwrap();
        let config = create_test_config(tmp_dir.path());

        let store = ExperienceStore::new(config.clone()).await;
        if store.is_err() {
            tracing::warn!("Skipping test: Qdrant not available");
            return;
        }
        let store = store.unwrap();

        let exp = Experience::new(
            "Fix memory leak".to_string(),
            "bug_fix".to_string(),
            "Memory grew unbounded".to_string(),
            "Added connection pool limits".to_string(),
            vec!["bash".to_string(), "grep".to_string()],
            2,
            true,
            "embedding_1".to_string(),
        );

        store.store(&exp).await.unwrap();

        let retrieved = store.get_by_id(&exp.id).await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.task_description, "Fix memory leak");
        assert_eq!(retrieved.task_type, "bug_fix");
    }

    #[tokio::test]
    async fn test_search_by_task_type() {
        let tmp_dir = tempdir().unwrap();
        let config = create_test_config(tmp_dir.path());

        let store = ExperienceStore::new(config.clone()).await;
        if store.is_err() {
            tracing::warn!("Skipping test: Qdrant not available");
            return;
        }
        let store = store.unwrap();

        let exp1 = Experience::new(
            "Bug 1".to_string(),
            "bug_fix".to_string(),
            "Problem 1".to_string(),
            "Solution 1".to_string(),
            vec![],
            1,
            true,
            "embedding_1".to_string(),
        );

        let exp2 = Experience::new(
            "Feature 1".to_string(),
            "feature_impl".to_string(),
            "Problem 2".to_string(),
            "Solution 2".to_string(),
            vec![],
            1,
            true,
            "embedding_2".to_string(),
        );

        store.store(&exp1).await.unwrap();
        store.store(&exp2).await.unwrap();

        let bug_fixes = store.search_by_task_type("bug_fix", 10).await.unwrap();
        assert_eq!(bug_fixes.len(), 1);
        assert_eq!(bug_fixes[0].task_type, "bug_fix");
    }

    #[tokio::test]
    async fn test_count() {
        let tmp_dir = tempdir().unwrap();
        let config = create_test_config(tmp_dir.path());

        let store = ExperienceStore::new(config.clone()).await;
        if store.is_err() {
            tracing::warn!("Skipping test: Qdrant not available");
            return;
        }
        let store = store.unwrap();

        let count = store.count().await.unwrap();
        assert_eq!(count, 0);

        let exp = Experience::new(
            "Test".to_string(),
            "test".to_string(),
            "Problem".to_string(),
            "Solution".to_string(),
            vec![],
            1,
            true,
            "embedding_test".to_string(),
        );

        store.store(&exp).await.unwrap();
        let count = store.count().await.unwrap();
        assert_eq!(count, 1);
    }
}
