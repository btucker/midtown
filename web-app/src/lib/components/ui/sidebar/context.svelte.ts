import { getContext, setContext } from "svelte";
import { IsMobile } from "$lib/hooks/is-mobile.svelte.ts";
import {
	SIDEBAR_KEYBOARD_SHORTCUT,
	SIDEBAR_MAX_WIDTH,
	SIDEBAR_MIN_WIDTH,
	SIDEBAR_WIDTH_STORAGE_KEY,
} from "./constants.ts";

interface SidebarStateProps {
	open: () => boolean;
	setOpen: (open: boolean) => void;
}

class SidebarState {
	props: SidebarStateProps;
	open = $derived.by(() => this.props.open());
	openMobile = $state(false);
	setOpen: (open: boolean) => void;
	#isMobile: IsMobile;
	state = $derived.by(() => (this.open ? "expanded" : "collapsed"));
	width = $state(256); // Default width in pixels (16rem)
	isResizing = $state(false);

	constructor(props: SidebarStateProps) {
		this.setOpen = props.setOpen;
		this.#isMobile = new IsMobile();
		this.props = props;

		// Load saved width from localStorage
		if (typeof window !== "undefined") {
			const saved = localStorage.getItem(SIDEBAR_WIDTH_STORAGE_KEY);
			if (saved) {
				const parsed = parseInt(saved, 10);
				if (!Number.isNaN(parsed) && parsed >= SIDEBAR_MIN_WIDTH && parsed <= SIDEBAR_MAX_WIDTH) {
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
	handleShortcutKeydown = (e: KeyboardEvent) => {
		if (e.key === SIDEBAR_KEYBOARD_SHORTCUT && (e.metaKey || e.ctrlKey)) {
			e.preventDefault();
			this.toggle();
		}
	};

	setOpenMobile = (value: boolean) => {
		this.openMobile = value;
	};

	toggle = () => {
		return this.#isMobile.current ? (this.openMobile = !this.openMobile) : this.setOpen(!this.open);
	};

	setWidth = (newWidth: number) => {
		const clamped = Math.max(SIDEBAR_MIN_WIDTH, Math.min(SIDEBAR_MAX_WIDTH, newWidth));
		this.width = clamped;
		if (typeof window !== "undefined") {
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
export function setSidebar(props: SidebarStateProps): SidebarState {
	return setContext(Symbol.for(SYMBOL_KEY), new SidebarState(props));
}

/**
 * Retrieves the `SidebarState` instance from the context. This is a class instance,
 * so you cannot destructure it.
 * @returns The `SidebarState` instance.
 */
export function useSidebar(): SidebarState {
	return getContext<SidebarState>(Symbol.for(SYMBOL_KEY));
}
