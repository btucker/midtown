<script>
/**
 * EditBlock — renders an Edit tool call using the existing DiffView component.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 *           input.file_path — path being edited
 *           input.old_string — text that was replaced
 *           input.new_string — replacement text
 */
import DiffView from "./DiffView.svelte";

let { block } = $props();

let filePath = $derived(block.input?.file_path || "unknown");
let oldString = $derived(block.input?.old_string || "");
let newString = $derived(block.input?.new_string || "");
</script>

<DiffView {filePath} {oldString} {newString} />
