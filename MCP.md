# Model Context Protocol (MCP) Support in GeWu

GeWu supports the Model Context Protocol (MCP), allowing it to connect to external tool servers. This enables GeWu to use tools that are not built directly into the CLI, such as filesystem access (via external server), GitHub integration, or custom Python scripts.

## Configuration

MCP servers are configured in `~/.gewu/mcp.json`.

The configuration file follows this structure:

```json
{
  "mcpServers": {
    "server-name": {
      "command": "executable-command",
      "args": ["arg1", "arg2"],
      "env": {
        "ENV_VAR": "value"
      }
    }
  }
}
```

### Example

Here is an example configuration that connects to a local Python script acting as an MCP server:

```json
{
  "mcpServers": {
    "my-python-tools": {
      "command": "python3",
      "args": ["/path/to/my/tools_server.py"],
      "env": {
        "PYTHONUNBUFFERED": "1"
      }
    },
    "github-mcp": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "your-token-here"
      }
    }
  }
}
```

## Usage

When `gewu-cli` starts, it automatically reads `~/.gewu/mcp.json`, connects to the configured servers, and loads their available tools. These tools are then made available to the agent alongside built-in tools.

If a server fails to start or initialize, `gewu-cli` will log a warning but continue running with the remaining available tools.

## Supported Features

- **Tool Discovery**: Automatically lists and registers tools from connected servers.
- **Tool Execution**: Forwards tool calls from the agent to the MCP server and returns the result.
- **Error Handling**: Gracefully handles server crashes or connection drops.
- **Server Logging**: Captures `notifications/message` from MCP servers and logs them via `tracing` (visible in CLI output with appropriate `--log-level`).

## Transports

GeWu supports two transport mechanisms for connecting to MCP servers:

### 1. Stdio (Local)

Spawns a local process and communicates via standard input/output. This is the default and most common way to use local tools.

Configuration:
```json
"server-name": {
  "command": "python3",
  "args": ["script.py"],
  "env": {}
}
```

### 2. SSE (Remote/HTTP)

Connects to a remote MCP server using Server-Sent Events (SSE) over HTTP. This is useful for connecting to servers running on different machines or in containers.

Configuration:
```json
"server-name": {
  "url": "http://localhost:8080/sse"
}
```
The client will connect to the SSE endpoint to receive events (including the message endpoint) and send requests via HTTP POST to the endpoint specified by the server.
