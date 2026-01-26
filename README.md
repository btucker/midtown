# Midtown

A multi-agent workspace management daemon for coordinating distributed AI agent workflows.

## Overview

Midtown (`midtownd`) is a daemon that manages multiple agent workspaces in a Git-based workflow system. It provides:

- **JSON-RPC 2.0** interface over Unix sockets for inter-process communication
- **Append-only channels** for durable message coordination between agents
- **Per-agent cursors** for tracking read positions in message streams

## Installation

### From Source

```bash
cargo build --release
```

The binary will be available at `target/release/midtownd`.

## Usage

### Starting the Daemon

```bash
# Start with default socket path (/tmp/midtown.sock)
midtownd

# Start with custom socket path
midtownd --socket /path/to/custom.sock

# Enable verbose logging
midtownd --verbose
```

### Command-Line Options

| Option | Short | Description | Default |
|--------|-------|-------------|---------|
| `--socket` | `-s` | Path to Unix socket | `/tmp/midtown.sock` |
| `--verbose` | `-v` | Enable debug logging | `false` |

### Stopping the Daemon

The daemon responds to:
- `SIGTERM` - Graceful shutdown
- `SIGINT` (Ctrl+C) - Graceful shutdown
- RPC `shutdown` method

## RPC API

Midtown uses JSON-RPC 2.0 over Unix sockets. Connect to the socket and send newline-delimited JSON requests.

### Methods

#### `ping`

Health check endpoint.

**Request:**
```json
{"jsonrpc": "2.0", "method": "ping", "id": 1}
```

**Response:**
```json
{"jsonrpc": "2.0", "result": "pong", "id": 1}
```

#### `version`

Get daemon version information.

**Request:**
```json
{"jsonrpc": "2.0", "method": "version", "id": 1}
```

**Response:**
```json
{"jsonrpc": "2.0", "result": {"name": "midtownd", "version": "0.1.0"}, "id": 1}
```

#### `shutdown`

Request daemon shutdown.

**Request:**
```json
{"jsonrpc": "2.0", "method": "shutdown", "id": 1}
```

**Response:**
```json
{"jsonrpc": "2.0", "result": {"status": "shutting_down"}, "id": 1}
```

### Example: Using socat

```bash
echo '{"jsonrpc":"2.0","method":"ping","id":1}' | socat - UNIX-CONNECT:/tmp/midtown.sock
```

### Error Codes

| Code | Description |
|------|-------------|
| -32700 | Parse error - Invalid JSON |
| -32600 | Invalid request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

## Development

### Building

```bash
cargo build
```

### Running Tests

```bash
cargo test
```

### Project Structure

```
src/
├── main.rs     # Daemon entry point and connection handling
├── lib.rs      # Library root with error types and public API
├── rpc.rs      # JSON-RPC 2.0 protocol types
├── channel.rs  # Append-only message log management
├── cursor.rs   # Per-agent read position tracking
└── message.rs  # Message types for channel communication
```

## License

MIT License - see [LICENSE](LICENSE) for details.
