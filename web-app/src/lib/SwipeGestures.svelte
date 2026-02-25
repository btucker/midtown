<script>
  import { useSidebar } from '$lib/components/ui/sidebar/context.svelte.js'
  import { threadData } from '$lib/store.js'
  import { closeThread } from '$lib/api.js'
  import { onMount } from 'svelte'

  const sidebar = useSidebar()

  // Swipe must start within this many px of the left edge
  const EDGE_THRESHOLD = 20
  // Minimum horizontal travel to count as a swipe
  const MIN_SWIPE_X = 50
  // Horizontal travel must exceed vertical by this ratio (avoids triggering on scrolls)
  const SWIPE_RATIO = 1.5

  onMount(() => {
    let startX = null
    let startY = null
    let tracking = false

    function onTouchStart(e) {
      if (!sidebar.isMobile) return
      const touch = e.touches[0]
      if (touch.clientX <= EDGE_THRESHOLD) {
        startX = touch.clientX
        startY = touch.clientY
        tracking = true
      }
    }

    function onTouchEnd(e) {
      if (!tracking) return
      tracking = false

      const touch = e.changedTouches[0]
      const deltaX = touch.clientX - startX
      const deltaY = Math.abs(touch.clientY - startY)

      if (deltaX >= MIN_SWIPE_X && deltaX >= deltaY * SWIPE_RATIO) {
        if ($threadData) {
          closeThread()
        } else if (!sidebar.openMobile) {
          sidebar.setOpenMobile(true)
        }
      }

      startX = null
      startY = null
    }

    function onTouchCancel() {
      tracking = false
      startX = null
      startY = null
    }

    document.addEventListener('touchstart', onTouchStart, { passive: true })
    document.addEventListener('touchend', onTouchEnd, { passive: true })
    document.addEventListener('touchcancel', onTouchCancel, { passive: true })

    return () => {
      document.removeEventListener('touchstart', onTouchStart)
      document.removeEventListener('touchend', onTouchEnd)
      document.removeEventListener('touchcancel', onTouchCancel)
    }
  })
</script>
