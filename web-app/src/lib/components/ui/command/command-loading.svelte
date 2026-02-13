<script>
	import { Command as CommandPrimitive } from "bits-ui";

	let { ref = $bindable(null), ...restProps } = $props();
</script>

<CommandPrimitive.Loading bind:ref {...restProps} />