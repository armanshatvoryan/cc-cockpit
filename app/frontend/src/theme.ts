// Theme — dark (default) / light, chosen in Settings (⌘,) and remembered.
//
// The whole UI palette lives in styles.css as custom properties; the only thing
// this module does for CSS is stamp `data-theme` on <html>, which flips the
// override block. It is applied at MODULE LOAD, before the first render, so a
// light-theme user never sees a frame of dark chrome on boot.
//
// The terminal is the exception. xterm paints into its own DOM with a JS theme
// object, so it cannot read the CSS variables — its two palettes are spelled
// out here and handed to every live Terminal when the theme changes. The ANSI
// set matters as much as the background: xterm's built-in palette is designed
// for a dark canvas (its "white" is #e5e5e5, invisible on a light one), so the
// light theme ships a full 16-colour set rather than just flipping bg/fg.
//
// Persistence is localStorage, not the disk-backed LayoutSnapshot — the theme
// is a pure view preference with no business in a versioned backend schema, and
// it must be readable synchronously at module load.

import { createSignal } from "solid-js";

export type ThemeName = "dark" | "light";

const STORAGE_KEY = "cc-cockpit.theme";

/** Read the saved theme; anything unrecognised (or a blocked localStorage) → dark. */
function readStored(): ThemeName {
  try {
    return localStorage.getItem(STORAGE_KEY) === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

const [theme, setThemeSignal] = createSignal<ThemeName>(readStored());

/** Stamp the root element — the `:root[data-theme="light"]` block keys off this. */
function applyToDocument(next: ThemeName): void {
  document.documentElement.setAttribute("data-theme", next);
}

// Applied on import, i.e. before `render()` runs in index.tsx.
applyToDocument(theme());

export { theme };

export function setTheme(next: ThemeName): void {
  if (next === theme()) return;
  setThemeSignal(next);
  applyToDocument(next);
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    // Private mode / storage disabled: the theme still applies for this run.
  }
}

export function toggleTheme(): void {
  setTheme(theme() === "dark" ? "light" : "dark");
}

/** xterm `ITheme`-shaped palettes. Kept structurally identical so a key can
 *  never exist in one theme and silently fall back to an xterm default in the
 *  other. */
export interface TermPalette {
  background: string;
  foreground: string;
  cursor: string;
  cursorAccent: string;
  selectionBackground: string;
  black: string;
  red: string;
  green: string;
  yellow: string;
  blue: string;
  magenta: string;
  cyan: string;
  white: string;
  brightBlack: string;
  brightRed: string;
  brightGreen: string;
  brightYellow: string;
  brightBlue: string;
  brightMagenta: string;
  brightCyan: string;
  brightWhite: string;
}

/** Dark: the original v1 terminal colours, plus xterm's own defaults spelled
 *  out explicitly so both themes are declared the same way. */
const DARK_TERM: TermPalette = {
  background: "#0d1017",
  foreground: "#dbe2ef",
  cursor: "#60a5fa",
  cursorAccent: "#0d1017",
  selectionBackground: "#1e3a5f",
  black: "#2e3440",
  red: "#f87171",
  green: "#34d399",
  yellow: "#fbbf24",
  blue: "#60a5fa",
  magenta: "#c084fc",
  cyan: "#22d3ee",
  white: "#dbe2ef",
  brightBlack: "#5b6680",
  brightRed: "#fca5a5",
  brightGreen: "#6ee7b7",
  brightYellow: "#fcd34d",
  brightBlue: "#93c5fd",
  brightMagenta: "#d8b4fe",
  brightCyan: "#67e8f9",
  brightWhite: "#f8fafc",
};

/** Light: a GitHub-light-style ANSI set — every colour is picked to stay legible
 *  on a white canvas, which the stock palette's white/bright-white are not. */
const LIGHT_TERM: TermPalette = {
  background: "#ffffff",
  foreground: "#1b1f27",
  cursor: "#2563eb",
  cursorAccent: "#ffffff",
  selectionBackground: "#cfe0ff",
  black: "#24292e",
  red: "#d73a49",
  green: "#22863a",
  yellow: "#b08800",
  blue: "#0366d6",
  magenta: "#6f42c1",
  cyan: "#1b7c83",
  white: "#6a737d",
  brightBlack: "#57606a",
  brightRed: "#cb2431",
  brightGreen: "#28a745",
  brightYellow: "#976800",
  brightBlue: "#0550ae",
  brightMagenta: "#8250df",
  brightCyan: "#1b7c83",
  brightWhite: "#24292e",
};

/** The palette for the current theme — read inside a reactive scope to have a
 *  terminal re-theme itself on the next flip. */
export function termPalette(): TermPalette {
  return theme() === "light" ? LIGHT_TERM : DARK_TERM;
}
