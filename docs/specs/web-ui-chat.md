# Web UI — Chat & Messaging

**Status:** Draft
**Last updated:** 2026-03-30

Behavioral spec for the chat and messaging experience in the Midtown web UI (Svelte PWA). Each requirement is a testable contract suitable for Playwright E2E tests.

---

## 1. Message Posting

- WHEN the user types text and presses Enter (or taps Send) THEN the message SHALL be sent to the active channel via WebSocket and appear immediately (optimistic rendering)
- WHEN the user presses Shift+Enter THEN a newline SHALL be inserted (no send)
- WHEN the message input is empty THEN the Send button SHALL be disabled
- WHEN the user pastes an image or file THEN a preview SHALL appear and the file SHALL be uploaded on send
- WHEN the active channel is a DM channel THEN the placeholder text SHALL show the peer agent's name

---

## 2. Autocomplete

### 2.1 @Mentions

- WHEN the user types `@` THEN an autocomplete dropdown SHALL appear showing agents relevant to the current context
- WHEN in a channel THEN the dropdown SHALL show the channel lead and all workers in that channel
- WHEN in a thread THEN the dropdown SHALL show the thread fork (if one exists), the channel lead, and workers assigned to tasks in that thread
- WHEN `@all` is shown THEN it SHALL always be available as an option
- WHEN the user types additional characters after `@` THEN the list SHALL filter in real time

### 2.2 !Task References

- WHEN the user types `!` THEN an autocomplete dropdown SHALL appear showing tasks relevant to the current context
- WHEN in a channel THEN the dropdown SHALL show tasks assigned to that channel
- WHEN in a thread THEN the dropdown SHALL show the parent task and its descendants

### 2.3 /Slash Commands

- WHEN the user types `/` at the start of a message THEN an autocomplete dropdown SHALL appear showing available commands
- WHEN commands are available THEN built-in commands SHALL be shown (`/archive`, `/unarchive`)
- WHEN plugin/skill commands are registered THEN they SHALL also appear in the dropdown (not yet implemented)

### 2.4 Shared Behavior

- WHEN the user selects an item (click or Enter) THEN it SHALL be inserted into the message text
- WHEN the user presses Escape or deletes the trigger character THEN the dropdown SHALL close
- WHEN the dropdown is open THEN arrow keys SHALL navigate the list and Enter SHALL select the highlighted item

---

## 3. Message Rendering

- WHEN a message contains markdown THEN it SHALL be rendered as HTML (headings, bold, italic, lists, links, tables)
- WHEN a message contains a fenced code block THEN it SHALL be syntax-highlighted using highlight.js
- WHEN a message contains a mermaid code block THEN it SHALL be rendered as a diagram
- WHEN a message contains `_underscore_words_` THEN they SHALL NOT be italicized (identifiers like `function_name` are preserved)
- WHEN a message contains an image URL THEN it SHALL render as a clickable image (lightbox on click)
- WHEN a message contains an uploaded file reference THEN it SHALL render as a downloadable link

---

## 4. Message Display

- WHEN consecutive messages are from the same sender within a short time window THEN the sender name and avatar SHALL be collapsed (only shown on the first message in the group)
- WHEN a new day boundary occurs between messages THEN a day divider SHALL be displayed
- WHEN a message is from the system (`midtown` sender) THEN it SHALL be styled distinctly (system message appearance)
- WHEN a message has replies THEN a reply count badge SHALL be shown, clickable to open the thread
- WHEN an agent message includes tool activity THEN tool blocks SHALL be rendered inline:
  - Bash commands: show the command and output
  - Edit blocks: show the file path and diff
  - Read blocks: show the file path
  - TodoWrite blocks: show the task list
  - Other tools: show a generic block with tool name and summary

---

## 5. Scrolling & Windowed Rendering

- WHEN the channel loads THEN the most recent 100 messages SHALL be rendered (windowed)
- WHEN the user scrolls to the top THEN 50 more messages SHALL be loaded and prepended
- WHEN new messages arrive AND the user is scrolled to the bottom THEN the view SHALL auto-scroll to show the new message
- WHEN new messages arrive AND the user has scrolled up THEN the view SHALL NOT auto-scroll (preserve reading position)

---

## 6. Agent Interaction

- WHEN the user clicks an agent's name in a message AND the agent has a DM channel THEN the app SHALL navigate to that agent's DM channel
- WHEN the user clicks an agent's name in a message AND the agent has no DM channel THEN no navigation SHALL occur
- WHEN the user clicks the stop button on a running lead THEN a cancel request SHALL be sent to stop the lead agent
- WHEN a message is from a dimmed sender (system, inactive agents) THEN the message text SHALL be muted

---

## 7. Slash Commands

- WHEN the user types `/archive` and sends THEN the current channel SHALL be archived and the user navigated away
- WHEN the user types `/unarchive <name>` and sends THEN the named channel SHALL be unarchived
- WHEN a slash command succeeds THEN a success message SHALL be shown in the chat
- WHEN a slash command fails THEN an error message SHALL be shown in the chat

---

## 8. Thread Replies (from Channel view)

- WHEN a message has replies AND the user clicks the reply count THEN the thread panel SHALL open
- WHEN the user clicks the reply icon on a message THEN the thread panel SHALL open for that message
- WHEN the user is on mobile AND taps a message with replies THEN the thread panel SHALL open
- WHEN the user is on mobile AND taps a message without replies THEN the message SHALL show action options (reply, link)
