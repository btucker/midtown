// @ts-expect-error — Svelte component re-export; buttonVariants is defined in the <script> block
import Root, { buttonVariants } from "./button.svelte";

export {
	buttonVariants,
	Root,
	//
	Root as Button,
};
