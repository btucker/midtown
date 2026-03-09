const COLLAPSE_DELAY_MS = 30_000;

/**
 * Compute the initial display state based on message age.
 * @param {string|null} timestamp — ISO 8601 timestamp of the parent message
 * @returns {'preview' | 'collapsed'}
 */
export function computeInitialState(timestamp, now = Date.now()) {
	if (!timestamp) return "collapsed";
	const age = now - new Date(timestamp).getTime();
	return age >= COLLAPSE_DELAY_MS ? "collapsed" : "preview";
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
 *   const ac = createAutoCollapse(timestamp);
 *   let displayState = $state(ac.initial);
 *   $effect(() => {
 *       ac.startTimer(() => { displayState = "collapsed"; });
 *       return () => ac.clearTimer();
 *   });
 */
export function createAutoCollapse(timestamp) {
	const now = Date.now();
	const initial = computeInitialState(timestamp, now);
	let timerId = null;
	let timeoutMs = null;

	if (initial === "preview") {
		const age = now - new Date(timestamp).getTime();
		timeoutMs = Math.max(0, COLLAPSE_DELAY_MS - age);
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
				timerId = setTimeout(onCollapse, timeoutMs);
			}
		},
	};
}
