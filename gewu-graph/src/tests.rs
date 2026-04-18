#[cfg(test)]
mod tests {
    use crate::graph::{StateGraph, END};
    use crate::state::State;
    use futures::StreamExt;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct TestState {
        count: i32,
        messages: Vec<String>,
    }

    impl State for TestState {
        type Partial = PartialTestState;

        fn merge(&self, update: Self::Partial) -> Self {
            Self {
                count: update.count.unwrap_or(self.count),
                messages: update.messages.unwrap_or_else(|| self.messages.clone()),
            }
        }
    }

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    struct PartialTestState {
        count: Option<i32>,
        messages: Option<Vec<String>>,
    }

    #[tokio::test]
    async fn test_simple_graph() {
        let mut graph = StateGraph::<TestState>::new();

        let increment = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 1),
                messages: None,
            }
        };

        let add_message = |state: TestState| async move {
            let mut msgs = state.messages.clone();
            msgs.push(format!("count is {}", state.count));
            PartialTestState {
                count: None,
                messages: Some(msgs),
            }
        };

        graph.add_node("increment", increment);
        graph.add_node("add_message", add_message);
        graph.set_entry_point("increment");
        graph.add_edge("increment", "add_message");
        graph.set_finish_point("add_message");

        let compiled = graph.compile();
        let initial_state = TestState {
            count: 0,
            messages: vec![],
        };

        let result = compiled.invoke(initial_state).await.unwrap();

        assert_eq!(result.count, 1);
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0], "count is 1");
    }

    #[tokio::test]
    async fn test_conditional_edges() {
        let mut graph = StateGraph::<TestState>::new();

        let increment = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 1),
                messages: None,
            }
        };

        let decrement = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count - 1),
                messages: None,
            }
        };

        let check = |state: &TestState| -> String {
            if state.count >= 5 {
                "high".to_string()
            } else {
                "low".to_string()
            }
        };

        graph.add_node("increment", increment);
        graph.add_node("decrement", decrement);
        graph.set_entry_point("increment");
        graph.add_conditional_edges(
            "increment",
            check,
            [
                ("high".to_string(), "decrement".to_string()),
                ("low".to_string(), END.to_string()),
            ]
            .into(),
        );
        graph.set_finish_point("decrement");

        let compiled = graph.compile();

        let result1 = compiled
            .invoke(TestState {
                count: 10,
                messages: vec![],
            })
            .await
            .unwrap();
        assert_eq!(result1.count, 10);

        let result2 = compiled
            .invoke(TestState {
                count: 3,
                messages: vec![],
            })
            .await
            .unwrap();
        assert_eq!(result2.count, 4);
    }

    #[tokio::test]
    async fn test_stream() {
        let mut graph = StateGraph::<TestState>::new();

        let step1 = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 10),
                messages: None,
            }
        };

        let step2 = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 1),
                messages: None,
            }
        };

        graph.add_node("step1", step1);
        graph.add_node("step2", step2);
        graph.set_entry_point("step1");
        graph.add_edge("step1", "step2");
        graph.set_finish_point("step2");

        let compiled = graph.compile();
        let initial_state = TestState {
            count: 0,
            messages: vec![],
        };

        let stream = compiled.stream(initial_state);
        let mut stream = Box::pin(stream);
        let mut states = Vec::new();

        while let Some(result) = stream.next().await {
            states.push(result.unwrap());
        }

        assert_eq!(states.len(), 3);
        assert_eq!(states[0].count, 0);
        assert_eq!(states[1].count, 10);
        assert_eq!(states[2].count, 11);
    }

    #[tokio::test]
    async fn test_invoke_parallel() {
        let mut graph = StateGraph::<TestState>::new();

        let add_a = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 1),
                messages: None,
            }
        };

        let add_b = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 10),
                messages: None,
            }
        };

        let record_c = |state: TestState| async move {
            PartialTestState {
                count: None,
                messages: Some(vec![format!("c{}", state.count)]),
            }
        };

        graph.add_node("add_a", add_a);
        graph.add_node("add_b", add_b);
        graph.add_node("record_c", record_c);
        graph.set_entry_point("add_a");
        graph.add_edge("add_a", "add_b");
        graph.add_edge("add_a", "record_c");
        graph.set_finish_point("add_b");
        graph.set_finish_point("record_c");

        let compiled = graph.compile();
        let initial_state = TestState {
            count: 0,
            messages: vec![],
        };

        let result = compiled.invoke_parallel(initial_state).await.unwrap();

        assert_eq!(result.count, 11);
        assert_eq!(result.messages.len(), 1);
    }

    #[tokio::test]
    async fn test_subgraph() {
        let mut subgraph = StateGraph::<TestState>::new();

        let double = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count * 2),
                messages: None,
            }
        };

        let add_hello = |state: TestState| async move {
            let mut msgs = state.messages.clone();
            msgs.push("hello".to_string());
            PartialTestState {
                count: None,
                messages: Some(msgs),
            }
        };

        subgraph.add_node("double", double);
        subgraph.add_node("add_hello", add_hello);
        subgraph.set_entry_point("double");
        subgraph.add_edge("double", "add_hello");
        subgraph.set_finish_point("add_hello");

        let subgraph_compiled = subgraph.compile();

        let mut main_graph = StateGraph::<TestState>::new();

        let increment = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count + 1),
                messages: None,
            }
        };

        let subtract = |state: TestState| async move {
            PartialTestState {
                count: Some(state.count - 1),
                messages: None,
            }
        };

        main_graph.add_node("increment", increment);
        main_graph.add_subgraph_node("nested", subgraph_compiled);
        main_graph.add_node("subtract", subtract);
        main_graph.set_entry_point("increment");
        main_graph.add_edge("increment", "nested");
        main_graph.add_edge("nested", "subtract");
        main_graph.set_finish_point("subtract");

        let compiled = main_graph.compile();
        let initial_state = TestState {
            count: 5,
            messages: vec![],
        };

        let result = compiled.invoke(initial_state).await.unwrap();

        assert_eq!(result.count, 11);
        assert_eq!(result.messages.len(), 1);
        assert_eq!(result.messages[0], "hello");
    }
}
