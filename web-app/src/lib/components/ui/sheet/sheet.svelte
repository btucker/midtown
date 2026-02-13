<script>
	import { Dialog as SheetPrimitive } from "bits-ui";

	let { open = $bindable(false), ...restProps } = $props();
</script>

<SheetPrimitive.Root bind:open {...restProps} />