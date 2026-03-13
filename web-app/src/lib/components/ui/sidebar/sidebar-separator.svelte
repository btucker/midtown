<script>
import { Separator } from "$lib/components/ui/separator/index.ts";
import { cn } from "$lib/utils.ts";

let { ref = $bindable(null), class: className, ...restProps } = $props();
</script>

<Separator
	bind:ref
	data-slot="sidebar-separator"
	data-sidebar="separator"
	class={cn("bg-sidebar-border", className)}
	{...restProps}
/>