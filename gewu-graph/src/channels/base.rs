use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

#[async_trait]
pub trait BaseChannel: Send + Sync {
    async fn read(&self) -> serde_json::Value;
    async fn write(&self, value: serde_json::Value) -> bool;
    fn is_empty(&self) -> bool;
}

pub struct LastValueChannel<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> {
    value: Arc<RwLock<Option<T>>>,
}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> LastValueChannel<T> {
    pub fn new() -> Self {
        Self {
            value: Arc::new(RwLock::new(None)),
        }
    }
}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> Default
    for LastValueChannel<T>
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> BaseChannel
    for LastValueChannel<T>
{
    async fn read(&self) -> serde_json::Value {
        let guard = self.value.read().await;
        match guard.as_ref() {
            Some(v) => serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    async fn write(&self, value: serde_json::Value) -> bool {
        if let Ok(t) = serde_json::from_value::<T>(value) {
            let mut guard = self.value.write().await;
            *guard = Some(t);
            true
        } else {
            false
        }
    }

    fn is_empty(&self) -> bool {
        false
    }
}

pub struct EphemeralValueChannel<
    T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static,
> {
    value: Arc<RwLock<Option<T>>>,
}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> EphemeralValueChannel<T> {
    pub fn new() -> Self {
        Self {
            value: Arc::new(RwLock::new(None)),
        }
    }
}

impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> Default
    for EphemeralValueChannel<T>
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> BaseChannel
    for EphemeralValueChannel<T>
{
    async fn read(&self) -> serde_json::Value {
        let mut guard = self.value.write().await;
        let value = guard.take();
        match value {
            Some(v) => serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    async fn write(&self, value: serde_json::Value) -> bool {
        if let Ok(t) = serde_json::from_value::<T>(value) {
            let mut guard = self.value.write().await;
            *guard = Some(t);
            true
        } else {
            false
        }
    }

    fn is_empty(&self) -> bool {
        false
    }
}

pub struct BinaryOperatorChannel<T: Clone + Send + Sync + 'static> {
    value: Arc<RwLock<Option<T>>>,
    op: Arc<dyn Fn(T, T) -> T + Send + Sync>,
}

impl<T: Clone + Send + Sync + 'static> BinaryOperatorChannel<T> {
    pub fn new<F>(op: F) -> Self
    where
        F: Fn(T, T) -> T + Send + Sync + 'static,
    {
        Self {
            value: Arc::new(RwLock::new(None)),
            op: Arc::new(op),
        }
    }

    pub fn sum() -> Self
    where
        T: std::ops::Add<Output = T>,
    {
        Self::new(|a, b| a + b)
    }
}

impl<T: Clone + Send + Sync + 'static> Default for BinaryOperatorChannel<T> {
    fn default() -> Self {
        Self::new(|_, b| b)
    }
}

#[async_trait]
impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> BaseChannel
    for BinaryOperatorChannel<T>
{
    async fn read(&self) -> serde_json::Value {
        let guard = self.value.read().await;
        match guard.as_ref() {
            Some(v) => serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    async fn write(&self, value: serde_json::Value) -> bool {
        if let Ok(t) = serde_json::from_value::<T>(value) {
            let mut guard = self.value.write().await;
            match guard.take() {
                Some(existing) => {
                    let op = self.op.clone();
                    *guard = Some(op(existing, t));
                }
                None => {
                    *guard = Some(t);
                }
            }
            true
        } else {
            false
        }
    }

    fn is_empty(&self) -> bool {
        false
    }
}

pub struct AnyValueChannel<T: Clone + Send + Sync + 'static> {
    value: Arc<RwLock<Option<T>>>,
    first_wins: Arc<RwLock<bool>>,
}

impl<T: Clone + Send + Sync + 'static> AnyValueChannel<T> {
    pub fn new() -> Self {
        Self {
            value: Arc::new(RwLock::new(None)),
            first_wins: Arc::new(RwLock::new(false)),
        }
    }

    pub fn clear(&self) {
        // Reset for next use
    }
}

impl<T: Clone + Send + Sync + 'static> Default for AnyValueChannel<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<T: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> BaseChannel
    for AnyValueChannel<T>
{
    async fn read(&self) -> serde_json::Value {
        let guard = self.value.read().await;
        match guard.as_ref() {
            Some(v) => serde_json::to_value(v).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    async fn write(&self, value: serde_json::Value) -> bool {
        let first_wins = self.first_wins.read().await;
        if *first_wins {
            return true;
        }
        drop(first_wins);

        if let Ok(t) = serde_json::from_value::<T>(value) {
            let mut guard = self.value.write().await;
            let mut first_wins = self.first_wins.write().await;
            if *first_wins {
                return true;
            }
            *guard = Some(t);
            *first_wins = true;
            true
        } else {
            false
        }
    }

    fn is_empty(&self) -> bool {
        false
    }
}

pub struct NamedBarrierChannel {
    waiters: Arc<RwLock<Vec<String>>>,
    expected: Arc<RwLock<HashSet<String>>>,
}

impl NamedBarrierChannel {
    pub fn new(expected: Vec<String>) -> Self {
        Self {
            waiters: Arc::new(RwLock::new(Vec::new())),
            expected: Arc::new(RwLock::new(expected.into_iter().collect())),
        }
    }

    pub async fn wait_for(&self, name: &str) -> bool {
        let mut waiters = self.waiters.write().await;
        waiters.push(name.to_string());

        drop(waiters);

        let expected = self.expected.read().await;
        let waiters = self.waiters.read().await;
        expected.iter().all(|e| waiters.contains(e))
    }
}

impl Default for NamedBarrierChannel {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

#[async_trait]
impl BaseChannel for NamedBarrierChannel {
    async fn read(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    async fn write(&self, _value: serde_json::Value) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        false
    }
}