// Lazy loader for the selkie WASM module.
// Caches the loaded module so init only runs once.
// On failure, clears the cached promise so subsequent calls can retry.

interface SelkieModule {
	render: (...args: unknown[]) => string;
}

let selkie: SelkieModule | null = null;
let initPromise: Promise<SelkieModule> | null = null;

export async function getSelkie(): Promise<SelkieModule> {
	if (selkie) return selkie;

	if (!initPromise) {
		initPromise = (async (): Promise<SelkieModule> => {
			const { default: initWasm, initialize, render } = await import("selkie-rs");
			await initWasm();
			initialize({ startOnLoad: false });
			selkie = { render: render as (...args: unknown[]) => string };
			return selkie!;
		})().catch((err) => {
			initPromise = null;
			throw err;
		});
	}

	return initPromise;
}
