<!-- Centered date label on a horizontal rule. Used for day dividers in channels and threads. -->
<script>
  let { label } = $props()
</script>

<div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem] select-none">
  <div class="flex-1 h-px bg-border/60"></div>
  <span>{label}</span>
  <div class="flex-1 h-px bg-border/60"></div>
</div>
