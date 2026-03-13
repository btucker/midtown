// Lazy loader for the mermaid rendering module.
// Caches the loaded module so init only runs once.
// On failure, clears the cached promise so subsequent calls can retry.

interface SelkieModule {
	render: (id: string, code: string) => Promise<{ svg: string }>;
}

let selkie: SelkieModule | null = null;
let initPromise: Promise<SelkieModule> | null = null;

export async function getSelkie(): Promise<SelkieModule> {
	if (selkie) return selkie;

	if (!initPromise) {
		initPromise = (async (): Promise<SelkieModule> => {
			const mermaid = (await import("mermaid")).default;
			mermaid.initialize({ startOnLoad: false });
			selkie = {
				render: async (id: string, code: string) => {
					const { svg } = await mermaid.render(id, code);
					return { svg };
				},
			};
			return selkie!;
		})().catch((err) => {
			initPromise = null;
			throw err;
		});
	}

	return initPromise;
}
