<script lang="ts">
import { Dialog as SheetPrimitive } from "bits-ui";
import { cn } from "$lib/utils.ts";

let { ref = $bindable(null), class: className, ...restProps } = $props();
</script>

<SheetPrimitive.Description
	bind:ref
	data-slot="sheet-description"
	class={cn("text-muted-foreground text-sm", className)}
	{...restProps}
/>