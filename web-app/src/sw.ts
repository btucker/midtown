/// <reference lib="webworker" />
declare const self: ServiceWorkerGlobalScope;

import { clientsClaim } from "workbox-core";
import { cleanupOutdatedCaches, precacheAndRoute } from "workbox-precaching";

// Take control of all pages immediately when activated
clientsClaim();

// Clean up old caches from previous versions
cleanupOutdatedCaches();

// Workbox precaching - the manifest is injected by vite-plugin-pwa at build time
precacheAndRoute(self.__WB_MANIFEST);

// Listen for SKIP_WAITING message from the client (sent by vite-plugin-pwa's registerSW)
// This allows immediate activation of new service workers
self.addEventListener("message", (event: ExtendableMessageEvent) => {
	if (event.data && event.data.type === "SKIP_WAITING") {
		self.skipWaiting();
	}
});

// Handle incoming push notifications
self.addEventListener("push", (event: PushEvent) => {
	if (!event.data) return;

	let data: { title?: string; body?: string; tag?: string; url?: string };
	try {
		data = event.data.json();
	} catch {
		data = { title: "Midtown", body: event.data.text() };
	}

	const title = data.title || "Midtown";
	const options = {
		body: data.body || "",
		icon: "/pwa-192x192.png",
		badge: "/pwa-192x192.png",
		tag: data.tag || "default",
		renotify: true,
		data: {
			url: data.url || "/",
		},
	};

	// Skip notification if a client window is focused (user is already in the app)
	event.waitUntil(
		self.clients
			.matchAll({ type: "window", includeUncontrolled: true })
			.then((windowClients: readonly WindowClient[]) => {
				const hasFocusedClient = windowClients.some(
					(client: WindowClient) => client.visibilityState === "visible" && client.focused,
				);
				if (hasFocusedClient) return;
				return self.registration.showNotification(title, options);
			}),
	);
});

// Handle notification click - open or focus the PWA and navigate to the deep-link URL
self.addEventListener("notificationclick", (event: NotificationEvent) => {
	event.notification.close();

	const targetUrl = event.notification.data?.url || "/";

	event.waitUntil(
		self.clients
			.matchAll({ type: "window", includeUncontrolled: true })
			.then((windowClients: readonly WindowClient[]) => {
				for (const client of windowClients) {
					if (client.url.includes(self.location.origin) && "focus" in client) {
						// Use postMessage instead of client.navigate() for cross-platform
						// reliability (Safari PWA support for navigate() is spotty).
						// The app registers a listener that handles navigation using its
						// own stores, avoiding a full page reload.
						return client.focus().then((focusedClient) => {
							if (targetUrl && targetUrl !== "/") {
								focusedClient.postMessage({ type: "NAVIGATE", url: targetUrl });
							}
						});
					}
				}
				return self.clients.openWindow(targetUrl);
			}),
	);
});
