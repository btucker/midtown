<script>
import { Command as CommandPrimitive } from "bits-ui";
import { cn } from "$lib/utils.ts";

let { api = $bindable(null), ref = $bindable(null), value = $bindable(""), class: className, ...restProps } = $props();
</script>

<CommandPrimitive.Root
	bind:this={api}
	bind:value
	bind:ref
	data-slot="command"
	class={cn(
		"bg-popover text-popover-foreground flex h-full w-full flex-col overflow-hidden rounded-md",
		className
	)}
	{...restProps}
/>