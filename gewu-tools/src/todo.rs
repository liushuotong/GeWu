use crate::fs_utils::into_tool_error;
use anyhow::Result;
use async_trait::async_trait;
use gewu_core::traits::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::error::Error;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub content: String,
    pub status: String,
    pub priority: Option<u8>,
}

pub struct TodoWriteTool {
    pub todos: Arc<Mutex<Vec<TodoItem>>>,
}

pub struct TodoReadTool {
    pub todos: Arc<Mutex<Vec<TodoItem>>>,
}

impl TodoWriteTool {
    pub fn new(todos: Arc<Mutex<Vec<TodoItem>>>) -> Self {
        Self { todos }
    }
}

impl TodoReadTool {
    pub fn new(todos: Arc<Mutex<Vec<TodoItem>>>) -> Self {
        Self { todos }
    }
}

fn lock_err() -> Box<dyn Error + Send + Sync> {
    anyhow::anyhow!("Todo list lock is poisoned").into()
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todowrite"
    }

    fn description(&self) -> &str {
        "Use this tool to update your todo list with the current tasks and their status"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "content": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["in_progress", "completed", "pending"],
                                "description": "The current status of the todo"
                            },
                            "priority": {
                                "type": "number",
                                "description": "Priority level (1 = highest, 3 = lowest)"
                            }
                        },
                        "required": ["id", "content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn call(&self, args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let args_val: Value = serde_json::from_str(args).map_err(into_tool_error)?;
        let todos_input = args_val["todos"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("todos is required"))
            .map_err(into_tool_error)?;

        let todos: Vec<TodoItem> = todos_input
            .iter()
            .map(|item| TodoItem {
                id: item["id"].as_str().unwrap_or_default().to_string(),
                content: item["content"].as_str().unwrap_or_default().to_string(),
                status: item["status"].as_str().unwrap_or("pending").to_string(),
                priority: item["priority"].as_u64().map(|p| p as u8),
            })
            .collect();

        let mut todos_guard = self.todos.lock().map_err(|_| lock_err())?;
        *todos_guard = todos;

        let non_completed = todos_guard
            .iter()
            .filter(|t| t.status != "completed")
            .count();
        Ok(format!("{} todos", non_completed))
    }
}

#[async_trait]
impl Tool for TodoReadTool {
    fn name(&self) -> &str {
        "todoread"
    }

    fn description(&self) -> &str {
        "Use this tool to read your todo list"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn call(&self, _args: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let todos_guard = self.todos.lock().map_err(|_| lock_err())?;
        let non_completed = todos_guard
            .iter()
            .filter(|t| t.status != "completed")
            .count();
        Ok(format!(
            "{} todos\n\n{}",
            non_completed,
            serde_json::to_string_pretty(&*todos_guard).map_err(into_tool_error)?
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_todo_write_and_read() {
        let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
        let write_tool = TodoWriteTool::new(todos.clone());
        let read_tool = TodoReadTool::new(todos.clone());

        let input = json!({
            "todos": [
                {"id": "1", "content": "Write code", "status": "in_progress", "priority": 1},
                {"id": "2", "content": "Write tests", "status": "pending", "priority": 2},
                {"id": "3", "content": "Done task", "status": "completed", "priority": 3}
            ]
        });

        let result = write_tool.call(&input.to_string()).await.unwrap();
        assert_eq!(result, "2 todos");

        let read_result = read_tool.call("{}").await.unwrap();
        assert!(read_result.contains("2 todos"));
        assert!(read_result.contains("Write code"));
        assert!(read_result.contains("Write tests"));
        assert!(read_result.contains("Done task"));
    }

    #[tokio::test]
    async fn test_todo_read_empty() {
        let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
        let read_tool = TodoReadTool::new(todos);

        let result = read_tool.call("{}").await.unwrap();
        assert!(result.contains("0 todos"));
        assert!(result.contains("[]"));
    }

    #[tokio::test]
    async fn test_todo_write_empty_list() {
        let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
        let write_tool = TodoWriteTool::new(todos.clone());

        let input = json!({
            "todos": []
        });

        let result = write_tool.call(&input.to_string()).await.unwrap();
        assert_eq!(result, "0 todos");
    }

    #[tokio::test]
    async fn test_todo_status_filtering() {
        let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
        let write_tool = TodoWriteTool::new(todos.clone());
        let read_tool = TodoReadTool::new(todos);

        let input = json!({
            "todos": [
                {"id": "1", "content": "Task 1", "status": "completed"},
                {"id": "2", "content": "Task 2", "status": "in_progress"},
                {"id": "3", "content": "Task 3", "status": "pending"},
                {"id": "4", "content": "Task 4", "status": "completed"}
            ]
        });

        write_tool.call(&input.to_string()).await.unwrap();
        let result = read_tool.call("{}").await.unwrap();

        assert!(result.contains("2 todos"));
    }
}
