<script>
	import { cn } from "$lib/utils.js";
	import { useSidebar } from "./context.svelte.js";

	let {
		ref = $bindable(null),
		class: className,
		...restProps
	} = $props();

	const sidebar = useSidebar();
	let startX = 0;
	let startWidth = 0;

	function handleMouseDown(e) {
		if (sidebar.isMobile) return; // Don't allow resize on mobile

		e.preventDefault();
		startX = e.clientX;
		startWidth = sidebar.width;
		sidebar.startResize();

		document.addEventListener('mousemove', handleMouseMove);
		document.addEventListener('mouseup', handleMouseUp);
		document.body.style.cursor = 'ew-resize';
		document.body.style.userSelect = 'none';
	}

	function handleMouseMove(e) {
		const delta = e.clientX - startX;
		const newWidth = startWidth + delta;
		sidebar.setWidth(newWidth);
	}

	function handleMouseUp() {
		sidebar.stopResize();
		document.removeEventListener('mousemove', handleMouseMove);
		document.removeEventListener('mouseup', handleMouseUp);
		document.body.style.cursor = '';
		document.body.style.userSelect = '';
	}

	// Cleanup listeners if component is destroyed mid-drag
	$effect(() => {
		return () => {
			document.removeEventListener('mousemove', handleMouseMove);
			document.removeEventListener('mouseup', handleMouseUp);
			document.body.style.cursor = '';
			document.body.style.userSelect = '';
		};
	});
</script>

<div
	bind:this={ref}
	role="separator"
	aria-label="Resize sidebar"
	data-sidebar="resize-handle"
	data-slot="resize-handle"
	onmousedown={handleMouseDown}
	class={cn(
		"absolute inset-y-0 -right-1 w-2 cursor-ew-resize hover:bg-primary/20 transition-colors z-30",
		"hidden md:block",
		sidebar.isResizing && "bg-primary/30",
		className
	)}
	{...restProps}
>
	<!-- Visual indicator on hover and during resize -->
	<div class={cn(
		"absolute inset-y-0 left-0 w-full flex items-center justify-center transition-opacity",
		sidebar.isResizing ? "opacity-100" : "opacity-0 hover:opacity-100"
	)}>
		<div class="w-0.5 h-12 bg-primary/60 rounded-full"></div>
	</div>
</div>
