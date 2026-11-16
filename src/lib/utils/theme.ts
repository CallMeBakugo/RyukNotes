import { darkThemes } from '../platform.ts';
import type { CustomTheme } from '../types';

const CUSTOM_THEME_VARS = [
	'--bg-primary', '--bg-secondary', '--bg-tertiary',
	'--bg-hover', '--bg-active', '--bg-editor',
	'--text-primary', '--text-secondary', '--border-color',
	'--border-light', '--text-tertiary',
];

// A custom theme carries nine colors, but the stylesheet also reads --border-light and
// --text-tertiary. Named themes set those in their own :root[data-theme] block; a custom theme
// has no such block, so without these they fall back to the light defaults in app.css and a dark
// custom theme draws near-white dividers. Derive them from the closest color the theme does have.
export function applyCustomThemeVars(root: HTMLElement, ct: CustomTheme) {
	root.style.setProperty('--bg-primary', ct.colors.bg_primary);
	root.style.setProperty('--bg-secondary', ct.colors.bg_secondary);
	root.style.setProperty('--bg-tertiary', ct.colors.bg_tertiary);
	root.style.setProperty('--bg-hover', ct.colors.bg_hover);
	root.style.setProperty('--bg-active', ct.colors.bg_active);
	root.style.setProperty('--bg-editor', ct.colors.bg_editor);
	root.style.setProperty('--text-primary', ct.colors.text_primary);
	root.style.setProperty('--text-secondary', ct.colors.text_secondary);
	root.style.setProperty('--border-color', ct.colors.border_color);
	root.style.setProperty('--border-light', ct.colors.border_color);
	root.style.setProperty('--text-tertiary', ct.colors.text_secondary);
}

function clearCustomThemeVars(root: HTMLElement) {
	for (const v of CUSTOM_THEME_VARS) root.style.removeProperty(v);
}

export function applyTheme(t: string, themes: CustomTheme[] = [], root: HTMLElement = document.documentElement) {
	const namedThemes = ['solarized-light', 'solarized-dark', 'catppuccin', 'nord', 'tokyo-night', 'github-light', 'github-dark', 'dracula', 'blueberry', 'forest-green', 'gruvbox', 'midnight-tide', 'cherry-blossom', 'synthwave', 'ember', 'moonlit', 'light-coffee', 'dark-coffee', 'cotton-candy', 'crimson', 'cloud', 'peach', 'material-dark', 'material-light', 'monokai', 'rose-pine', 'everforest', 'horizon', 'cyberpunk', 'black', 'one-dark'];
	root.classList.remove('dark');
	root.removeAttribute('data-theme');
	clearCustomThemeVars(root);
	if (t.startsWith('custom-')) {
		const ct = themes.find(c => c.id === t);
		if (ct) {
			applyCustomThemeVars(root, ct);
			if (ct.is_dark) root.classList.add('dark');
		}
	} else if (namedThemes.includes(t)) {
		root.setAttribute('data-theme', t);
		if (darkThemes.includes(t)) root.classList.add('dark');
	} else if (t === 'dark' || (t === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)) {
		root.classList.add('dark');
	}
}
