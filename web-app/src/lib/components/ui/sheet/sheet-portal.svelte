<script>
	import { Dialog as SheetPrimitive } from "bits-ui";

	let { ...restProps } = $props();
</script>

<SheetPrimitive.Portal {...restProps} />