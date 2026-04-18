use serde::{de::DeserializeOwned, Serialize};
use std::fmt::Debug;

pub trait State: Serialize + DeserializeOwned + Default + Debug + Clone + Send + Sync {
    type Partial: Serialize + DeserializeOwned + Debug + Clone + Send + Sync + Default;

    fn merge(&self, update: Self::Partial) -> Self;
}

pub trait Reducer<T>: Send + Sync + 'static {
    fn reduce(&self, current: T, new: T) -> T;
}

#[derive(Debug, Clone, Default)]
pub struct LastValue;

impl<T: Clone + Send + Sync> Reducer<T> for LastValue {
    fn reduce(&self, _current: T, new: T) -> T {
        new
    }
}

#[derive(Debug, Clone, Default)]
pub struct Append;

impl<T: Clone + Send + Sync> Reducer<Vec<T>> for Append {
    fn reduce(&self, mut current: Vec<T>, new: Vec<T>) -> Vec<T> {
        current.extend(new);
        current
    }
}

#[derive(Debug, Clone, Default)]
pub struct Sum;

impl Reducer<i32> for Sum {
    fn reduce(&self, current: i32, new: i32) -> i32 {
        current + new
    }
}

impl Reducer<i64> for Sum {
    fn reduce(&self, current: i64, new: i64) -> i64 {
        current + new
    }
}

impl Reducer<f64> for Sum {
    fn reduce(&self, current: f64, new: f64) -> f64 {
        current + new
    }
}
