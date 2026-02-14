import { IsMobile } from "$lib/hooks/is-mobile.svelte.js";
import { getContext, setContext } from "svelte";
import { SIDEBAR_KEYBOARD_SHORTCUT, SIDEBAR_WIDTH_STORAGE_KEY, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH } from "./constants.js";

class SidebarState {
	 props;
	open = $derived.by(() => this.props.open());
	openMobile = $state(false);
	setOpen;
	#isMobile;
	state = $derived.by(() => (this.open ? "expanded" : "collapsed"));
	width = $state(256); // Default width in pixels (16rem)
	isResizing = $state(false);

	constructor(props) {
		this.setOpen = props.setOpen;
		this.#isMobile = new IsMobile();
		this.props = props;

		// Load saved width from localStorage
		if (typeof window !== 'undefined') {
			const saved = localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY);
			if (saved) {
				const parsed = parseInt(saved, 10);
				if (!isNaN(parsed) && parsed >= SIDEBAR_MIN_WIDTH && parsed <= SIDEBAR_MAX_WIDTH) {
					this.width = parsed;
				}
			}
		}
	}

	// Convenience getter for checking if the sidebar is mobile
	// without this, we would need to use `sidebar.isMobile.current` everywhere
	get isMobile() {
		return this.#isMobile.current;
	}

	// Event handler to apply to the `<svelte:window>`
	handleShortcutKeydown = (e) => {
		if (e.key === SIDEBAR_KEYBOARD_SHORTCUT && (e.metaKey || e.ctrlKey)) {
			e.preventDefault();
			this.toggle();
		}
	};

	setOpenMobile = (value) => {
		this.openMobile = value;
	};

	toggle = () => {
		return this.#isMobile.current
			? (this.openMobile = !this.openMobile)
			: this.setOpen(!this.open);
	};

	setWidth = (newWidth) => {
		const clamped = Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, newWidth));
		this.width = clamped;
		if (typeof window !== 'undefined') {
			localStorage.setItem(SIDEBAR_WIDTH_STORAGE_KEY, String(clamped));
		}
	};

	startResize = () => {
		this.isResizing = true;
	};

	stopResize = () => {
		this.isResizing = false;
	};
}

const SYMBOL_KEY = "scn-sidebar";

/**
 * Instantiates a new `SidebarState` instance and sets it in the context.
 *
 * @param props The constructor props for the `SidebarState` class.
 * @returns  The `SidebarState` instance.
 */
export function setSidebar(props) {
	return setContext(Symbol.for(SYMBOL_KEY), new SidebarState(props));
}

/**
 * Retrieves the `SidebarState` instance from the context. This is a class instance,
 * so you cannot destructure it.
 * @returns The `SidebarState` instance.
 */
export function useSidebar() {
	return getContext(Symbol.for(SYMBOL_KEY));
}