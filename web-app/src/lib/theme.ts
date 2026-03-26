import { writable } from "svelte/store";

const STORAGE_KEY = "midtown-theme";

function getInitialTheme(): "light" | "dark" {
	const stored = typeof localStorage !== "undefined" ? localStorage.getItem(STORAGE_KEY) : null;
	if (stored === "light" || stored === "dark") return stored;
	if (typeof window !== "undefined" && window.matchMedia) {
		return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
	}
	return "dark";
}

export const theme = writable(getInitialTheme());

export function toggleTheme() {
	theme.update((t) => {
		const next = t === "dark" ? "light" : "dark";
		localStorage.setItem(STORAGE_KEY, next);
		return next;
	});
}
