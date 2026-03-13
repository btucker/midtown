declare module "virtual:pwa-register" {
	export function registerSW(options?: {
		onNeedRefresh?: () => void;
		onOfflineReady?: () => void;
		onRegistered?: (registration: ServiceWorkerRegistration | undefined) => void;
		onRegisterError?: (error: unknown) => void;
	}): (reloadPage?: boolean) => Promise<void>;
}

declare module "*.svelte" {
	import type { Component } from "svelte";
	const component: Component;
	export default component;
}
