use serde::{de::DeserializeOwned, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct TopicChannel<T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static> {
    inner: Arc<RwLock<VecDeque<T>>>,
    max_size: usize,
}

impl<T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static> TopicChannel<T> {
    pub fn new(max_size: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(VecDeque::new())),
            max_size,
        }
    }

    pub async fn read(&self) -> Option<T> {
        let guard = self.inner.read().await;
        guard.front().cloned()
    }

    pub async fn write(&self, value: T) {
        let mut guard = self.inner.write().await;
        guard.push_back(value);
        while guard.len() > self.max_size {
            guard.pop_front();
        }
    }
}

impl<T: Clone + Send + Sync + Serialize + DeserializeOwned + 'static> Default
    for TopicChannel<T>
{
    fn default() -> Self {
        Self::new(100)
    }
}