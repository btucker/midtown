export const DEFAULT_COLLAPSE_DELAY_MS = 10_000;

/**
 * Compute the initial display state based on message age.
 * @param {string|null} timestamp — ISO 8601 timestamp of the parent message
 * @param {number} now — current time in ms since epoch
 * @param {number} delay — collapse delay in ms
 * @returns {'preview' | 'collapsed'}
 */
export function computeInitialState(timestamp, now = Date.now(), delay = DEFAULT_COLLAPSE_DELAY_MS) {
	if (!timestamp) return "collapsed";
	const age = now - new Date(timestamp).getTime();
	return age >= delay ? "collapsed" : "preview";
}

/**
 * Create auto-collapse state for a tool block.
 *
 * Returns an object with:
 *   - initial: the computed initial state ('preview' | 'collapsed')
 *   - timeoutMs: remaining ms before collapse (null if already collapsed)
 *   - clearTimer(): cancel the pending timeout
 *   - startTimer(onCollapse): schedule the collapse callback
 *
 * Usage in a Svelte 5 component:
 *   const ac = $derived.by(() => createAutoCollapse(timestamp));
 *   $effect.pre(() => { if (!userOverride) displayState = ac.initial; });
 *   $effect(() => {
 *       if (userOverride) return;
 *       const currentAc = ac;
 *       currentAc.startTimer(() => { displayState = "collapsed"; });
 *       return () => currentAc.clearTimer();
 *   });
 */
export function createAutoCollapse(timestamp, delay = DEFAULT_COLLAPSE_DELAY_MS) {
	const now = Date.now();
	const initial = computeInitialState(timestamp, now, delay);
	let timerId = null;
	let timeoutMs = null;

	if (initial === "preview") {
		const age = now - new Date(timestamp).getTime();
		timeoutMs = Math.max(0, delay - age);
	}

	return {
		initial,
		timeoutMs,
		clearTimer() {
			if (timerId != null) {
				clearTimeout(timerId);
				timerId = null;
			}
		},
		startTimer(onCollapse) {
			if (timeoutMs != null) {
				if (timerId != null) clearTimeout(timerId);
				timerId = setTimeout(onCollapse, timeoutMs);
			}
		},
	};
}
