<script>
import { cn } from "$lib/utils.ts";

let { ref = $bindable(null), class: className, children, ...restProps } = $props();
</script>

<div
	bind:this={ref}
	data-slot="sheet-footer"
	class={cn("mt-auto flex flex-col gap-2 p-4", className)}
	{...restProps}
>
	{@render children?.()}
</div>