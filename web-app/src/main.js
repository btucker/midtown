import "./app.css";
import { registerSW } from "virtual:pwa-register";
import { mount } from "svelte";
import App from "./App.svelte";

const app = mount(App, {
	target: document.getElementById("app"),
});

// Svelte 5 mount() appends to the target (unlike Svelte 4 which replaced
// innerHTML), so the static #app-loader spinner from index.html persists.
document.getElementById("app-loader")?.remove();

// Register service worker with auto-update
// When a new version is available, prompt user before updating
const updateSW = registerSW({
	onNeedRefresh() {
		// Let user choose when to update to avoid losing unsaved work
		if (confirm("A new version is available. Reload to update?")) {
			// Call updateSW to send SKIP_WAITING message to the new service worker,
			// which triggers skipWaiting() and then reloads the page
			updateSW(true);
		}
	},
	onOfflineReady() {
		console.log("App ready to work offline");
	},
});

export default app;
