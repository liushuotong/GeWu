use crate::config::Config;
use crate::document::chunker::{ChildChunk, DocumentChunker, Bm25SparseVector};
use crate::retrieval::parent_store::ParentStore;
use crate::retrieval::qdrant_store::QdrantStore;
use std::path::Path;
use tracing::{info, instrument};

pub struct DocumentIndexer {
    config: Config,
    chunker: DocumentChunker,
    vector_store: QdrantStore,
    parent_store: ParentStore,
}

impl DocumentIndexer {
    pub async fn new(config: Config) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let chunker = DocumentChunker::new(config.clone());
        let vector_store = QdrantStore::new(config.clone()).await?;
        let parent_store = ParentStore::new(config.parent_store_path.clone());

        Ok(Self {
            config,
            chunker,
            vector_store,
            parent_store,
        })
    }

    #[instrument(skip(self, markdown_dir))]
    pub async fn index_markdown_dir(
        &mut self,
        markdown_dir: impl AsRef<Path>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let dir = markdown_dir.as_ref();
        if !dir.exists() {
            tracing::warn!("Markdown directory does not exist: {:?}", dir);
            return Ok(());
        }

        let pattern_path = dir.join("*.md");
        let pattern_str = pattern_path.to_str().unwrap_or("");
        let entries = glob::glob(pattern_str)?
            .filter_map(|e| e.ok())
            .collect::<Vec<_>>();

        if entries.is_empty() {
            tracing::info!("No markdown files found in {:?}", dir);
            return Ok(());
        }

        let mut all_contents: Vec<String> = Vec::new();
        let mut all_child_chunks: Vec<ChildChunk> = Vec::new();

        for md_path in &entries {
            info!("Reading file: {:?}", md_path);
            let content = std::fs::read_to_string(md_path)?;
            let source_name = md_path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let parent_chunks = self.chunker.create_parent_chunks(&content, &source_name);

            for parent in &parent_chunks {
                let children = self.chunker.create_child_chunks(parent, &source_name);
                all_child_chunks.extend(children);
                all_contents.push(parent.content.clone());
            }

            for parent in &parent_chunks {
                self.parent_store.save(parent)?;
            }
        }

        if self.config.enable_sparse {
            info!("Building IDF for BM25 sparse vectors...");
            self.chunker.build_idf(&all_contents);

            let avg_doc_len = if all_contents.is_empty() {
                self.config.avg_doc_length
            } else {
                all_contents.iter().map(|c| c.len()).sum::<usize>() / all_contents.len()
            };
            self.chunker.set_avg_doc_length(avg_doc_len as f32);
            info!("Average document length: {}", avg_doc_len);
        }

        let sparse_vectors: Option<Vec<Bm25SparseVector>> = if self.config.enable_sparse {
            info!("Computing sparse vectors for {} chunks", all_child_chunks.len());
            let mut sparse = Vec::new();
            for chunk in &all_child_chunks {
                let sv = self.chunker.compute_sparse(&chunk.content);
                sparse.push(sv);
            }
            Some(sparse)
        } else {
            None
        };

        info!("Adding {} chunks to vector store", all_child_chunks.len());
        self.vector_store.add_chunks(&all_child_chunks, sparse_vectors.as_deref()).await?;

        if self.config.enable_sparse {
            let bm25_path = self.config.parent_store_path.join("bm25_state.json");
            info!("Saving BM25 state to {:?}", bm25_path);
            self.chunker.save_bm25_state(&bm25_path)?;
        }

        info!("Indexing complete!");
        Ok(())
    }

    #[instrument(skip(self, md_path))]
    pub async fn index_markdown_file(
        &mut self,
        md_path: impl AsRef<Path>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let path = md_path.as_ref();
        let content = std::fs::read_to_string(path)?;
        let source_name = path.file_stem().map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let parent_chunks = self.chunker.create_parent_chunks(&content, &source_name);

        let mut all_child_chunks: Vec<ChildChunk> = Vec::new();
        let mut all_contents: Vec<String> = Vec::new();

        for parent in &parent_chunks {
            let children = self.chunker.create_child_chunks(parent, &source_name);
            all_child_chunks.extend(children);
            all_contents.push(parent.content.clone());
        }

        let sparse_vectors: Option<Vec<Bm25SparseVector>> = if self.config.enable_sparse {
            self.chunker.build_idf(&all_contents);
            let mut sparse = Vec::new();
            for chunk in &all_child_chunks {
                sparse.push(self.chunker.compute_sparse(&chunk.content));
            }
            Some(sparse)
        } else {
            None
        };

        if !all_child_chunks.is_empty() {
            self.vector_store.add_chunks(&all_child_chunks, sparse_vectors.as_deref()).await?;
        }

        for parent in parent_chunks {
            self.parent_store.save(&parent)?;
        }

        info!(
            "Indexed {} child chunks",
            all_child_chunks.len()
        );

        Ok(())
    }

    pub async fn delete_collection(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.vector_store.delete_collection().await
    }

    pub async fn recreate_collection(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.vector_store.delete_collection().await?;
        self.vector_store.ensure_collection().await?;
        Ok(())
    }
}

pub fn pdf_to_markdown(
    pdf_path: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    Ok(output_dir.as_ref().join(pdf_path.as_ref().file_stem().unwrap()).with_extension("md"))
}
