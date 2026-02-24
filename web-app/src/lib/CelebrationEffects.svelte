<script>
  import { onDestroy } from 'svelte'
  import { kanbanData, daemonStatus } from './store.js'
  import { AVENUE_COLORS } from './avenue-colors.js'

  const COLOR_PALETTE = [
    'var(--color-primary)',
    'var(--color-accent)',
    'var(--color-foreground)',
    'var(--color-link-default)',
    'var(--color-link-task)',
    'var(--color-link-pr)',
    'var(--color-link-coworker)',
    'var(--color-insight)',
    ...Object.values(AVENUE_COLORS),
  ]

  const EMOJIS = ['🎉', '✨', '🥳', '🚀', '🌈', '💫', '⭐', '🎊']

  const celebratedPrs = new Set()
  const timers = new Map()
  let destroyed = false
  let hydrated = $state(false)
  let activeEffects = $state([])

  const EFFECT_DEFS = [
    { type: 'confetti', duration: 4200, generator: generateConfetti },
    { type: 'emoji', duration: 3800, generator: generateEmojiRain },
    { type: 'fireworks', duration: 3400, generator: generateFireworks },
    { type: 'matrix', duration: 3600, generator: generateMatrixCascade },
    { type: 'stars', duration: 3200, generator: generateStarScatter },
    { type: 'bubbles', duration: 3600, generator: generateBubbles },
    { type: 'ticker', duration: 3600, generator: generateTickerTape },
    { type: 'comets', duration: 3400, generator: generateComets },
    { type: 'pixels', duration: 3200, generator: generatePixelBurst },
    { type: 'ripples', duration: 3000, generator: generateRipples },
  ]

  function prKey(pr) {
    const repo = pr?.repo || 'default'
    return `${repo}#${pr?.number ?? 'unknown'}`
  }

  function randomFraction() {
    if (typeof crypto !== 'undefined' && crypto.getRandomValues) {
      const arr = new Uint32Array(1)
      crypto.getRandomValues(arr)
      return arr[0] / 4294967296 // 2^32 to keep result in [0,1)
    }
    return Math.random()
  }

  function randomInRange(min, max) {
    return min + (max - min) * randomFraction()
  }

  function pickColor() {
    const index = Math.floor(randomFraction() * COLOR_PALETTE.length)
    return COLOR_PALETTE[index]
  }

  function pickEmoji() {
    const index = Math.floor(randomFraction() * EMOJIS.length)
    return EMOJIS[index]
  }

  function triggerCelebration(pr) {
    if (EFFECT_DEFS.length === 0) return
    const index = Math.floor(randomFraction() * EFFECT_DEFS.length)
    const def = EFFECT_DEFS[index]
    const payload = def.generator()
    const idBase = typeof performance !== 'undefined' && performance.now ? performance.now() : Date.now()
    const id = `${def.type}-${idBase}-${Math.round(randomFraction() * 1e6)}`
    const entry = { id, type: def.type, payload }
    activeEffects = [...activeEffects, entry]
    const timer = setTimeout(() => removeEffect(entry.id), def.duration)
    timers.set(entry.id, timer)
  }

  function removeEffect(id) {
    if (destroyed) return
    activeEffects = activeEffects.filter((effect) => effect.id !== id)
    const timer = timers.get(id)
    if (timer) {
      clearTimeout(timer)
      timers.delete(id)
    }
  }

  onDestroy(() => {
    destroyed = true
    timers.forEach((timer) => clearTimeout(timer))
    timers.clear()
  })

  function remember(pr) {
    celebratedPrs.add(prKey(pr))
  }

  $effect(() => {
    const ready = Boolean($daemonStatus)
    if (!ready) return
    const done = $kanbanData.done || []
    if (!hydrated) {
      done.forEach((pr) => remember(pr))
      hydrated = true
      return
    }
    for (const pr of done) {
      const key = prKey(pr)
      if (!celebratedPrs.has(key)) {
        remember(pr)
        triggerCelebration(pr)
      }
    }
  })

  $effect(() => {
    if (!$daemonStatus) {
      celebratedPrs.clear()
      hydrated = false
      activeEffects = []
      timers.forEach((timer) => clearTimeout(timer))
      timers.clear()
    }
  })

  function generateConfetti() {
    return {
      particles: Array.from({ length: 36 }, () => ({
        left: randomInRange(0, 100),
        delay: randomInRange(0, 0.6),
        size: randomInRange(8, 14),
        rotation: randomInRange(-80, 80),
        color: pickColor(),
      })),
    }
  }

  function generateEmojiRain() {
    return {
      drops: Array.from({ length: 22 }, () => ({
        emoji: pickEmoji(),
        left: randomInRange(5, 95),
        delay: randomInRange(0, 0.7),
        duration: randomInRange(2.4, 3.4),
      })),
    }
  }

  function generateFireworks() {
    return {
      bursts: Array.from({ length: 3 }, (_, idx) => ({
        x: randomInRange(20, 80),
        y: randomInRange(25, 60),
        color: pickColor(),
        delay: idx * 0.2,
        scale: randomInRange(0.8, 1.2),
      })),
    }
  }

  function randomMatrixString() {
    const glyphs = '01MIDTOWN'
    const length = 12 + Math.floor(randomFraction() * 6)
    let result = ''
    for (let i = 0; i < length; i += 1) {
      const idx = Math.floor(randomFraction() * glyphs.length)
      result += glyphs[idx]
    }
    return result
  }

  function generateMatrixCascade() {
    return {
      columns: Array.from({ length: 14 }, () => ({
        left: randomInRange(0, 100),
        delay: randomInRange(0, 0.5),
        duration: randomInRange(2.6, 3.4),
        content: randomMatrixString(),
      })),
    }
  }

  function generateStarScatter() {
    return {
      stars: Array.from({ length: 24 }, () => ({
        x: randomInRange(10, 90),
        y: randomInRange(10, 70),
        delay: randomInRange(0, 0.4),
        scale: randomInRange(0.6, 1.4),
        color: pickColor(),
      })),
    }
  }

  function generateBubbles() {
    return {
      bubbles: Array.from({ length: 18 }, () => ({
        left: randomInRange(5, 95),
        size: randomInRange(18, 36),
        delay: randomInRange(0, 0.6),
        duration: randomInRange(2.6, 3.2),
        color: pickColor(),
      })),
    }
  }

  function generateTickerTape() {
    return {
      ribbons: Array.from({ length: 8 }, (_, idx) => ({
        top: randomInRange(10, 80),
        delay: idx * 0.2,
        direction: idx % 2 === 0 ? 'left' : 'right',
        color: pickColor(),
      })),
    }
  }

  function generateComets() {
    return {
      comets: Array.from({ length: 6 }, () => ({
        startX: randomInRange(0, 100),
        startY: randomInRange(0, 40),
        delay: randomInRange(0, 0.6),
        angle: randomInRange(20, 70),
        color: pickColor(),
      })),
    }
  }

  function generatePixelBurst() {
    return {
      pixels: Array.from({ length: 28 }, () => ({
        angle: randomInRange(0, 360),
        distance: randomInRange(60, 140),
        delay: randomInRange(0, 0.4),
        color: pickColor(),
      })),
    }
  }

  function generateRipples() {
    return {
      ripples: Array.from({ length: 4 }, () => ({
        x: randomInRange(30, 70),
        y: randomInRange(30, 70),
        delay: randomInRange(0, 0.3),
        color: pickColor(),
      })),
    }
  }
</script>

<div class="celebration-layer" aria-hidden="true">
  {#each activeEffects as effect (effect.id)}
    <div class={`celebration celebration-${effect.type}`}>
      {#if effect.type === 'confetti'}
        {#each effect.payload.particles as particle}
          <span
            class="confetti-piece"
            style={`--x:${particle.left}%;--delay:${particle.delay}s;--size:${particle.size}px;--rotate:${particle.rotation}deg;--confetti-color:${particle.color};`}
          ></span>
        {/each}
      {:else if effect.type === 'emoji'}
        {#each effect.payload.drops as drop}
          <span
            class="emoji-drop"
            style={`--x:${drop.left}%;--delay:${drop.delay}s;--duration:${drop.duration}s;`}
          >{drop.emoji}</span>
        {/each}
      {:else if effect.type === 'fireworks'}
        {#each effect.payload.bursts as burst}
          <div
            class="firework"
            style={`--x:${burst.x}%;--y:${burst.y}%;--firework-color:${burst.color};--delay:${burst.delay}s;--scale:${burst.scale};`}
          >
            {#each Array(12) as _, index}
              <span style={`--index:${index};`}></span>
            {/each}
          </div>
        {/each}
      {:else if effect.type === 'matrix'}
        {#each effect.payload.columns as column}
          <span
            class="matrix-column"
            style={`--x:${column.left}%;--delay:${column.delay}s;--duration:${column.duration}s;`}
          >{column.content}</span>
        {/each}
      {:else if effect.type === 'stars'}
        {#each effect.payload.stars as star}
          <span
            class="star"
            style={`--x:${star.x}%;--y:${star.y}%;--delay:${star.delay}s;--scale:${star.scale};--star-color:${star.color};`}
          >✶</span>
        {/each}
      {:else if effect.type === 'bubbles'}
        {#each effect.payload.bubbles as bubble}
          <span
            class="bubble"
            style={`--x:${bubble.left}%;--delay:${bubble.delay}s;--duration:${bubble.duration}s;--bubble-size:${bubble.size}px;--bubble-color:${bubble.color};`}
          ></span>
        {/each}
      {:else if effect.type === 'ticker'}
        {#each effect.payload.ribbons as ribbon}
          <span
            class={`ticker ${ribbon.direction === 'left' ? 'ticker-left' : 'ticker-right'}`}
            style={`--y:${ribbon.top}%;--delay:${ribbon.delay}s;--ticker-color:${ribbon.color};`}
          ></span>
        {/each}
      {:else if effect.type === 'comets'}
        {#each effect.payload.comets as comet}
          <span
            class="comet"
            style={`--x:${comet.startX}%;--y:${comet.startY}%;--delay:${comet.delay}s;--angle:${comet.angle}deg;--comet-color:${comet.color};`}
          ></span>
        {/each}
      {:else if effect.type === 'pixels'}
        {#each effect.payload.pixels as pixel}
          <span
            class="pixel"
            style={`--angle:${pixel.angle}deg;--distance:${pixel.distance}px;--delay:${pixel.delay}s;--pixel-color:${pixel.color};`}
          ></span>
        {/each}
      {:else if effect.type === 'ripples'}
        {#each effect.payload.ripples as ripple}
          <span
            class="ripple"
            style={`--x:${ripple.x}%;--y:${ripple.y}%;--delay:${ripple.delay}s;--ripple-color:${ripple.color};`}
          ></span>
        {/each}
      {/if}
    </div>
  {/each}
</div>

<style>
  .celebration-layer {
    pointer-events: none;
    position: fixed;
    inset: 0;
    padding: env(safe-area-inset-top) env(safe-area-inset-right) env(safe-area-inset-bottom) env(safe-area-inset-left);
    z-index: 60;
    overflow: hidden;
  }

  .celebration {
    position: absolute;
    inset: 0;
  }

  .confetti-piece {
    position: absolute;
    top: -10%;
    left: var(--x);
    width: var(--size);
    height: calc(var(--size) * 0.4);
    background: var(--confetti-color);
    border-radius: 2px;
    opacity: 0;
    animation: confetti-fall 3.4s linear forwards;
    animation-delay: var(--delay);
    transform: rotate(var(--rotate));
    box-shadow: 0 0 6px rgba(0, 0, 0, 0.12);
  }

  @keyframes confetti-fall {
    0% { transform: translateY(-20vh) rotate(var(--rotate)); opacity: 0; }
    10% { opacity: 1; }
    100% { transform: translateY(110vh) rotate(calc(var(--rotate) + 180deg)); opacity: 0; }
  }

  .emoji-drop {
    position: absolute;
    top: -10%;
    left: var(--x);
    font-size: 1.5rem;
    animation: emoji-rain var(--duration) ease-in forwards;
    animation-delay: var(--delay);
  }

  @keyframes emoji-rain {
    0% { transform: translateY(-10vh) scale(0.8); opacity: 0; }
    10% { opacity: 1; }
    100% { transform: translateY(105vh) scale(1.2); opacity: 0; }
  }

  .firework {
    position: absolute;
    left: var(--x);
    top: var(--y);
    width: 140px;
    height: 140px;
    margin-left: -70px;
    margin-top: -70px;
    transform: scale(var(--scale));
    opacity: 0;
    animation: firework-pop 0.2s ease-out forwards;
    animation-delay: var(--delay);
  }

  @keyframes firework-pop {
    0% { opacity: 0; transform: scale(0.3); }
    100% { opacity: 1; transform: scale(var(--scale)); }
  }

  .firework span {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 2px;
    height: 45%;
    background: var(--firework-color);
    transform-origin: bottom center;
    animation: firework-burst 0.8s ease-out forwards;
    animation-delay: calc(var(--delay) + var(--index) * 0.02s);
  }

  @keyframes firework-burst {
    0% { transform: rotate(calc(var(--index) * 30deg)) scaleY(0.2); opacity: 0; }
    40% { opacity: 1; }
    100% { transform: rotate(calc(var(--index) * 30deg)) scaleY(1.1) translateY(-15%); opacity: 0; }
  }

  .matrix-column {
    position: absolute;
    left: var(--x);
    top: -140%;
    font-family: 'IBM Plex Mono', monospace;
    font-size: 0.85rem;
    color: var(--color-link-coworker);
    text-shadow: 0 0 6px rgba(0, 0, 0, 0.5);
    animation: matrix-fall var(--duration) linear forwards;
    animation-delay: var(--delay);
  }

  @keyframes matrix-fall {
    0% { transform: translateY(-10vh); opacity: 0; }
    15% { opacity: 1; }
    100% { transform: translateY(120vh); opacity: 0; }
  }

  .star {
    position: absolute;
    left: var(--x);
    top: var(--y);
    color: var(--star-color);
    font-size: 1.2rem;
    opacity: 0;
    animation: star-pop 1.2s ease-out forwards;
    animation-delay: var(--delay);
    transform: translate(-50%, -50%) scale(var(--scale));
  }

  @keyframes star-pop {
    0% { opacity: 0; transform: translate(-50%, -50%) scale(0.4); }
    40% { opacity: 1; }
    100% { opacity: 0; transform: translate(-50%, -50%) scale(1.8); }
  }

  .bubble {
    position: absolute;
    left: var(--x);
    bottom: -15%;
    width: var(--bubble-size);
    height: var(--bubble-size);
    border: 2px solid var(--bubble-color);
    border-radius: 50%;
    opacity: 0;
    animation: bubble-rise var(--duration) ease-out forwards;
    animation-delay: var(--delay);
  }

  @keyframes bubble-rise {
    0% { transform: translateY(20vh) scale(0.6); opacity: 0; }
    20% { opacity: 0.6; }
    100% { transform: translateY(-100vh) scale(1.2); opacity: 0; }
  }

  .ticker {
    position: absolute;
    width: 45%;
    height: 12px;
    background: var(--ticker-color);
    opacity: 0.3;
    animation-duration: 2.4s;
    animation-fill-mode: forwards;
    animation-timing-function: ease-in-out;
    animation-delay: var(--delay);
  }

  .ticker-left {
    top: var(--y);
    left: -50%;
    animation-name: ticker-left;
  }

  .ticker-right {
    top: var(--y);
    right: -50%;
    animation-name: ticker-right;
  }

  @keyframes ticker-left {
    0% { transform: translateX(0); opacity: 0; }
    50% { opacity: 0.5; }
    100% { transform: translateX(220%); opacity: 0; }
  }

  @keyframes ticker-right {
    0% { transform: translateX(0); opacity: 0; }
    50% { opacity: 0.5; }
    100% { transform: translateX(-220%); opacity: 0; }
  }

  .comet {
    position: absolute;
    left: var(--x);
    top: var(--y);
    width: 120px;
    height: 2px;
    background: linear-gradient(90deg, var(--comet-color), transparent);
    transform: rotate(var(--angle));
    transform-origin: left center;
    opacity: 0;
    animation: comet-fly 1.4s ease-out forwards;
    animation-delay: var(--delay);
  }

  @keyframes comet-fly {
    0% { opacity: 0; transform: rotate(var(--angle)) translateX(-80px); }
    20% { opacity: 1; }
    100% { opacity: 0; transform: rotate(var(--angle)) translateX(220px); }
  }

  .pixel {
    position: absolute;
    left: 50%;
    top: 50%;
    width: 6px;
    height: 6px;
    background: var(--pixel-color);
    opacity: 0;
    animation: pixel-burst 0.9s ease-out forwards;
    animation-delay: var(--delay);
    transform-origin: center;
  }

  @keyframes pixel-burst {
    0% { opacity: 0; transform: translate(-50%, -50%) rotate(0deg) translateY(0px) scale(0.4); }
    20% { opacity: 1; }
    100% { opacity: 0; transform: translate(-50%, -50%) rotate(var(--angle)) translateY(calc(-1 * var(--distance))) scale(0.2); }
  }

  .ripple {
    position: absolute;
    left: var(--x);
    top: var(--y);
    width: 20px;
    height: 20px;
    border: 2px solid var(--ripple-color);
    border-radius: 50%;
    transform: translate(-50%, -50%);
    opacity: 0;
    animation: ripple-expand 1.4s ease-out forwards;
    animation-delay: var(--delay);
  }

  @keyframes ripple-expand {
    0% { opacity: 0.5; transform: translate(-50%, -50%) scale(0.2); }
    80% { opacity: 0.2; }
    100% { opacity: 0; transform: translate(-50%, -50%) scale(6); }
  }
</style>
