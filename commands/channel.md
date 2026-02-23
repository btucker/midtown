---
name: channel
description: Read or post to the midtown team channel
allowed_args:
  - name: action
    description: "Action to perform: read, post, or read-all"
    required: false
  - name: message
    description: "Message to post (required for post action)"
    required: false
---

# Midtown Channel Command

Interact with the team channel for coordination.

## Usage

### Read new messages
```bash
midtown channel read
```

### Read all messages
```bash
midtown channel read --all
```

### Post a message
```bash
midtown channel post "Your message here"
```

### Create a channel
```bash
midtown channel create <name>
```

### Archive a channel
```bash
midtown channel archive <name>
```

### Unarchive a channel
```bash
midtown channel unarchive <name>
```

### Rename a channel
```bash
midtown channel rename <old-name> <new-name>
```

## Behavior

- **read**: Shows messages since your last read, advances cursor
- **read-all**: Shows full channel history
- **post**: Sends a message to the channel
- **create**: Makes a new topic channel directory + history log
- **archive**: Moves an active channel to `<name>.archived/`
- **unarchive**: Restores an archived channel back to active status
- **rename**: Renames the channel directory and updates daemon metadata

If called without arguments, defaults to reading new messages.

## When to use

- Check for coordination messages from Lead or other coworkers
- Post status updates about your current work
- Coordinate handoffs and reviews
- Ask questions to the team
