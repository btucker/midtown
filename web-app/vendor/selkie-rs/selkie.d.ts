/* tslint:disable */
/* eslint-disable */

/**
 * Mirror mermaid-js's initialize API (currently a no-op).
 */
export function initialize(_config: any): void;

/**
 * Validate a Mermaid diagram and return an error on failure.
 */
export function parse(input: string): void;

/**
 * Render Mermaid diagram text to SVG with a mermaid-js compatible return shape.
 */
export function render(id: string, input: string): any;

/**
 * Render Mermaid diagram text to SVG (WASM-friendly).
 */
export function render_text(input: string): string;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly initialize: (a: number) => void;
  readonly parse: (a: number, b: number, c: number) => void;
  readonly render: (a: number, b: number, c: number, d: number, e: number) => void;
  readonly render_text: (a: number, b: number, c: number) => void;
  readonly __wbindgen_export: (a: number) => void;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_export2: (a: number, b: number) => number;
  readonly __wbindgen_export3: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
*
* @returns {InitOutput}
*/
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
