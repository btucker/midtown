<script>
  let { svgHtml, onclose } = $props()

  let scale = $state(1)
  let translateX = $state(0)
  let translateY = $state(0)

  // Drag state
  let dragging = false
  let dragStartX = 0
  let dragStartY = 0
  let dragStartTranslateX = 0
  let dragStartTranslateY = 0

  // Pinch state
  let lastPinchDist = 0
  let lastPinchMidX = 0
  let lastPinchMidY = 0

  const MIN_SCALE = 0.25
  const MAX_SCALE = 10

  let containerEl = $state(null)

  function clampScale(s) {
    return Math.min(MAX_SCALE, Math.max(MIN_SCALE, s))
  }

  function handleWheel(e) {
    e.preventDefault()

    // Trackpad pinch gestures fire wheel events with ctrlKey set
    const isPinchGesture = e.ctrlKey

    const rect = containerEl.getBoundingClientRect()
    const cursorX = e.clientX - rect.left
    const cursorY = e.clientY - rect.top

    let newScale
    if (isPinchGesture) {
      // ctrlKey + wheel = trackpad pinch — deltaY is the zoom delta
      const factor = 1 - e.deltaY * 0.01
      newScale = clampScale(scale * factor)
    } else {
      // Regular scroll wheel zoom
      const delta = e.deltaY > 0 ? 0.9 : 1.1
      newScale = clampScale(scale * delta)
    }

    // Zoom toward cursor position
    const ratio = newScale / scale
    translateX = cursorX - ratio * (cursorX - translateX)
    translateY = cursorY - ratio * (cursorY - translateY)
    scale = newScale
  }

  // Track whether mouse moved during a press to distinguish click from drag
  let mouseMovedDuringPress = false

  function handleMouseDown(e) {
    if (e.button !== 0) return
    e.preventDefault()
    dragging = true
    mouseMovedDuringPress = false
    dragStartX = e.clientX
    dragStartY = e.clientY
    dragStartTranslateX = translateX
    dragStartTranslateY = translateY
  }

  function handleMouseMove(e) {
    if (!dragging) return
    e.preventDefault()
    mouseMovedDuringPress = true
    translateX = dragStartTranslateX + (e.clientX - dragStartX)
    translateY = dragStartTranslateY + (e.clientY - dragStartY)
  }

  function handleMouseUp(e) {
    const wasDragging = dragging
    dragging = false
    // Close on click (no drag) in the empty area outside the diagram
    if (wasDragging && !mouseMovedDuringPress && e.target === containerEl) {
      onclose()
    }
  }

  function getTouchDist(t1, t2) {
    const dx = t1.clientX - t2.clientX
    const dy = t1.clientY - t2.clientY
    return Math.sqrt(dx * dx + dy * dy)
  }

  function handleTouchStart(e) {
    if (e.touches.length === 2) {
      e.preventDefault()
      dragging = false
      const t0 = e.touches[0]
      const t1 = e.touches[1]
      lastPinchDist = getTouchDist(t0, t1)
      lastPinchMidX = (t0.clientX + t1.clientX) / 2
      lastPinchMidY = (t0.clientY + t1.clientY) / 2
    } else if (e.touches.length === 1) {
      dragging = true
      mouseMovedDuringPress = false
      dragStartX = e.touches[0].clientX
      dragStartY = e.touches[0].clientY
      dragStartTranslateX = translateX
      dragStartTranslateY = translateY
    }
  }

  function handleTouchMove(e) {
    if (e.touches.length === 2) {
      e.preventDefault()
      const t0 = e.touches[0]
      const t1 = e.touches[1]
      const dist = getTouchDist(t0, t1)
      const midX = (t0.clientX + t1.clientX) / 2
      const midY = (t0.clientY + t1.clientY) / 2

      if (lastPinchDist > 0) {
        const rect = containerEl.getBoundingClientRect()
        const pinchCenterX = midX - rect.left
        const pinchCenterY = midY - rect.top

        const newScale = clampScale(scale * (dist / lastPinchDist))
        const ratio = newScale / scale

        translateX = pinchCenterX - ratio * (pinchCenterX - translateX) + (midX - lastPinchMidX)
        translateY = pinchCenterY - ratio * (pinchCenterY - translateY) + (midY - lastPinchMidY)
        scale = newScale
      }

      lastPinchDist = dist
      lastPinchMidX = midX
      lastPinchMidY = midY
    } else if (e.touches.length === 1 && dragging) {
      e.preventDefault()
      translateX = dragStartTranslateX + (e.touches[0].clientX - dragStartX)
      translateY = dragStartTranslateY + (e.touches[0].clientY - dragStartY)
    }
  }

  function handleTouchEnd(e) {
    if (e.touches.length < 2) {
      lastPinchDist = 0
    }
    if (e.touches.length === 1) {
      // Re-anchor drag state when transitioning from pinch back to single touch
      dragging = true
      dragStartX = e.touches[0].clientX
      dragStartY = e.touches[0].clientY
      dragStartTranslateX = translateX
      dragStartTranslateY = translateY
    } else if (e.touches.length === 0) {
      dragging = false
    }
  }

  function handleKeyDown(e) {
    if (e.key === 'Escape') {
      e.stopPropagation()
      onclose()
    }
  }

  function resetZoom() {
    scale = 1
    translateX = 0
    translateY = 0
  }

  function handleBackdropClick(e) {
    // Close only when clicking the backdrop itself, not the diagram
    if (e.target === e.currentTarget) {
      onclose()
    }
  }
</script>

<svelte:window onkeydown={handleKeyDown} />

<!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
<div class="modal-backdrop" onclick={handleBackdropClick}>
  <button class="close-btn" onclick={onclose} title="Close (Esc)" aria-label="Close">×</button>
  <div class="modal-toolbar">
    <span class="zoom-level">{Math.round(scale * 100)}%</span>
    <button class="toolbar-btn" onclick={resetZoom} title="Reset zoom">Reset</button>
  </div>

  <!-- svelte-ignore a11y_no_static_element_interactions a11y_no_noninteractive_element_interactions -->
  <div
    class="zoom-container"
    bind:this={containerEl}
    onwheel={handleWheel}
    onmousedown={handleMouseDown}
    onmousemove={handleMouseMove}
    onmouseup={handleMouseUp}
    onmouseleave={handleMouseUp}
    ontouchstart={handleTouchStart}
    ontouchmove={handleTouchMove}
    ontouchend={handleTouchEnd}
    role="img"
    aria-label="Mermaid diagram - use scroll to zoom, drag to pan"
  >
    <div
      class="diagram-content"
      style="transform: translate({translateX}px, {translateY}px) scale({scale}); transform-origin: 0 0;"
    >
      {@html svgHtml}
    </div>
  </div>
</div>

<style>
  .modal-backdrop {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.85);
    z-index: 1000;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
  }

  .modal-toolbar {
    position: absolute;
    top: 16px;
    right: 56px;
    display: flex;
    align-items: center;
    gap: 8px;
    z-index: 1001;
  }

  .zoom-level {
    color: #a0a0a0;
    font-size: 0.8rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    min-width: 3em;
    text-align: right;
  }

  .toolbar-btn {
    padding: 6px 14px;
    border: 1px solid #4a4a4a;
    border-radius: 4px;
    background: #2a2a2a;
    color: #d0d0d0;
    font-size: 0.8rem;
    cursor: pointer;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
  }

  .toolbar-btn:hover {
    border-color: #5fafaf;
    color: #5fafaf;
  }

  .close-btn {
    position: absolute;
    top: 12px;
    right: 12px;
    width: 36px;
    height: 36px;
    border: 1px solid #4a4a4a;
    border-radius: 50%;
    background: #2a2a2a;
    color: #d0d0d0;
    font-size: 1.25rem;
    line-height: 1;
    cursor: pointer;
    z-index: 1001;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
  }

  .close-btn:hover {
    border-color: #ff5f5f;
    color: #ff5f5f;
    background: #3a2020;
  }

  .zoom-container {
    width: 100%;
    height: 100%;
    overflow: hidden;
    cursor: grab;
    touch-action: none;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .zoom-container:active {
    cursor: grabbing;
  }

  .diagram-content {
    will-change: transform;
  }

  .diagram-content :global(svg) {
    max-width: none;
    max-height: none;
    display: block;
  }
</style>
