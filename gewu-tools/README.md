# gewu-tools

`gewu-tools` provides a collection of utility tools for the GeWu agent framework. It also includes support for loading MCP (Model Context Protocol) servers as tools.

## Tools

### Available Tools

- **`FsTool`**: Provides file system operations (read, write, delete, list).
- **`BashTool`**: Executes shell commands securely.
- **`GrepTool`**: Searches for patterns in files.
- **`WebFetchTool`**: Fetches content from URLs.
- **`SearchTool`**: Performs web searches.
- **`ListTool`**: Lists files in directories.

### MCP (Model Context Protocol)

`gewu-tools` supports loading external tools via MCP. MCP is an open standard that enables AI models to interact with external data and tools.

## Usage

To use these tools, simply add them to your agent's configuration:

```rust
use gewu_tools::{FsTool, BashTool, GrepTool};
use gewu_core::Tool;

let tools = vec![
    Box::new(FsTool::new()),
    Box::new(BashTool::new()),
    Box::new(GrepTool::new()),
];

let agent = Agent::builder()
    .tools(tools)
    // ...
    .build()?;
```
