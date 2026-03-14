<script lang="ts">
import { Dialog as SheetPrimitive } from "bits-ui";
import { cn } from "$lib/utils.ts";

let { ref = $bindable(null), class: className, ...restProps } = $props();
</script>

<SheetPrimitive.Title
	bind:ref
	data-slot="sheet-title"
	class={cn("text-foreground font-semibold", className)}
	{...restProps}
/>