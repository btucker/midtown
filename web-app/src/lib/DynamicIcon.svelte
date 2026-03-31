<script lang="ts">
import type { Component, Snippet } from "svelte";

let {
	name,
	size = 14,
	class: className = "",
	fallback,
	...rest
}: {
	name: string;
	size?: number;
	class?: string;
	fallback?: Snippet;
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
	import(/* @vite-ignore */ `@lucide/svelte/icons/${iconName}`)
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
{:else if fallback}
	{@render fallback()}
{/if}
