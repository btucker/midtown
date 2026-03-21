<script lang="ts">
import type { Component } from "svelte";

let {
	name,
	size = 14,
	class: className = "",
	...rest
}: {
	name: string;
	size?: number;
	class?: string;
	[key: string]: unknown;
} = $props();

let Comp: Component | null = $state(null);
let loadedName = $state("");

$effect(() => {
	const iconName = name;
	if (!iconName) {
		Comp = null;
		loadedName = "";
		return;
	}
	if (iconName === loadedName) return;
	import(`@lucide/svelte/icons/${iconName}`)
		.then((m) => {
			Comp = m.default;
			loadedName = iconName;
		})
		.catch(() => {
			Comp = null;
			loadedName = "";
		});
});
</script>

{#if Comp}
	<Comp {size} class={className} {...rest} />
{/if}
