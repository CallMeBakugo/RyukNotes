<script lang="ts" module>
	let diagramId = 0;
	// Mermaid has a shared configuration; serialize snapshot renders and restore it afterward.
	let diagramQueue = Promise.resolve();
</script>

<script lang="ts">
	import createDOMPurify from 'dompurify';
	import katex from 'katex';
	import 'katex/dist/katex.min.css';
	import { calloutGroup, calloutIcon, calloutLabel } from '$lib/editor/callouts';
	import { readSecretTitle } from '$lib/utils/secrets';

	let { html, label = 'Snapshot preview' }: { html: string; label?: string } = $props();
	let root = $state<HTMLElement>();
	let purifier: ReturnType<typeof createDOMPurify>;

	const documentTags = [
		'a', 'abbr', 'b', 'blockquote', 'br', 'caption', 'code', 'col', 'colgroup',
		'dd', 'del', 'details', 'div', 'dl', 'dt', 'em', 'figcaption', 'figure',
		'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'i', 'img', 'kbd', 'li',
		'mark', 'ol', 'p', 'pre', 'q', 'rp', 'rt', 'ruby', 's', 'samp', 'small',
		'span', 'strong', 'sub', 'summary', 'sup', 'table', 'tbody', 'td', 'tfoot',
		'th', 'thead', 'tr', 'u', 'ul', 'var', 'wbr',
	];
	const documentAttributes = [
		'alt', 'class', 'colspan', 'dir', 'height', 'href', 'lang', 'open', 'reversed',
		'rowspan', 'scope', 'span', 'src', 'start', 'style', 'title', 'value', 'width',
		'data-type', 'data-checked', 'data-size', 'data-language', 'data-secret-block',
		'data-math-inline', 'data-math-block', 'data-callout', 'data-callout-title',
		'data-callout-foldable', 'data-callout-folded',
		'data-wiki-link', 'data-path', 'data-title', 'data-aliased', 'data-bg-color',
		'data-pdf-src', 'data-pdf-name', 'data-page-break',
	];
	// Custom asset protocols are accepted only here, then checked specifically on images.
	const documentUris = /^(?:https?:|mailto:|tel:|asset:|imgproxy:|blob:|data:image\/(?:png|jpeg|gif|webp|avif|bmp);|#)/i;
	const formattingProperties = [
		'color', 'background-color', 'font-weight', 'font-style', 'font-size',
		'text-align', 'text-decoration-line', 'text-decoration-color', 'vertical-align',
	];

	function message(text: string): HTMLParagraphElement {
		const node = document.createElement('p');
		node.className = 'snapshot-message';
		node.textContent = text;
		return node;
	}

	function imageSourceAllowed(source: string): boolean {
		if (/[\u0000-\u0020\u007f]/.test(source)) return false;
		if (/^(?:asset|imgproxy):\/\//i.test(source)) return true;
		if (/^data:image\/(?:png|jpeg|gif|webp|avif|bmp);base64,[a-z\d+/=]+$/i.test(source)) return true;
		try {
			const url = new URL(source);
			if (url.protocol === 'blob:') return new URL(url.pathname).origin === window.location.origin;
			return ['http:', 'https:'].includes(url.protocol)
				&& ['asset.localhost', 'imgproxy.localhost'].includes(url.hostname)
				&& !url.username && !url.password && !url.port;
		} catch {
			return false;
		}
	}

	function keepFormatting(element: HTMLElement): void {
		const style = element.style;
		const values = formattingProperties.map((property) => [property, style.getPropertyValue(property)]);
		element.removeAttribute('style');
		for (const [property, value] of values) {
			// Only presentation properties; no URL-bearing CSS, variables, escapes or overlays.
			if (value && !/url\s*\(|var\s*\(|expression\s*\(|[\\@]/i.test(value)) {
				element.style.setProperty(property, value);
			}
		}
	}

	function prepareDocument(fragment: DocumentFragment): void {
		for (const element of fragment.querySelectorAll<HTMLElement>('*')) {
			keepFormatting(element);
			const language = element.tagName === 'CODE'
				? Array.from(element.classList).find((name) => /^language-[\w+-]+$/.test(name))
				: undefined;
			element.removeAttribute('class');
			if (language) element.className = language;
			if (element.hasAttribute('data-wiki-link')) {
				element.className = 'snapshot-wiki-link';
				element.title = `Wiki link to ${element.getAttribute('data-title') || element.textContent || ''} (inactive in snapshot preview)`;
				element.setAttribute('aria-disabled', 'true');
			}
			if (element.hasAttribute('data-page-break')) element.className = 'snapshot-page-break';
			// Snapshot links are readable but inert: inspection cannot navigate the live note.
			if (element.tagName === 'A') {
				const href = element.getAttribute('href');
				if (href) element.title = `${element.title ? element.title + ' — ' : ''}${href} (inactive in snapshot preview)`;
				element.removeAttribute('href');
				element.setAttribute('aria-disabled', 'true');
			}
		}
		for (const attachment of fragment.querySelectorAll<HTMLElement>('[data-pdf-src]')) {
			const name = attachment.getAttribute('data-pdf-name') || 'PDF attachment';
			const source = attachment.getAttribute('data-pdf-src') || '';
			attachment.prepend(message(`${name} — PDF attachment (inline PDF rendering is unavailable in snapshot previews). ${source}`));
		}
		for (const image of fragment.querySelectorAll<HTMLImageElement>('img')) {
			const source = image.getAttribute('src') || '';
			if (!imageSourceAllowed(source)) {
				image.replaceWith(message(`Image unavailable or blocked${image.alt ? ': ' + image.alt : '.'}`));
				continue;
			}
			image.crossOrigin = 'anonymous';
			image.loading = 'lazy';
			image.decoding = 'async';
			image.addEventListener('error', () => {
				image.replaceWith(message(`Image could not be loaded: ${image.alt || source}`));
			}, { once: true });
		}
		for (const item of fragment.querySelectorAll<HTMLElement>('li[data-type="taskItem"]')) {
			const body = document.createElement('div');
			while (item.firstChild) body.append(item.firstChild);
			const checkbox = document.createElement('input');
			checkbox.type = 'checkbox';
			checkbox.checked = item.getAttribute('data-checked') === 'true';
			checkbox.disabled = true;
			checkbox.setAttribute('aria-label', checkbox.checked ? 'Completed task' : 'Incomplete task');
			item.className = 'snapshot-task';
			item.append(checkbox, body);
		}
		for (const table of fragment.querySelectorAll('table')) {
			const scroll = document.createElement('div');
			scroll.className = 'snapshot-table';
			scroll.tabIndex = 0;
			scroll.setAttribute('role', 'region');
			scroll.setAttribute('aria-label', 'Snapshot table');
			table.replaceWith(scroll);
			scroll.append(table);
		}
	}

	function renderSecrets(fragment: DocumentFragment): void {
		for (const node of fragment.querySelectorAll<HTMLElement>('[data-secret-block]')) {
			const stored = node.getAttribute('data-secret-block') || '';
			node.replaceChildren();
			node.className = 'snapshot-secret';
			const title = document.createElement('strong');
			let payload = stored;
			let error = '';
			try {
				payload = decodeURIComponent(stored);
				JSON.parse(payload);
			} catch {
				error = 'The stored secret payload is malformed; it cannot be previewed.';
			}
			title.textContent = readSecretTitle(payload);
			node.append(title, message('Locked encrypted content. Secrets are not unlocked in history previews.'));
			if (error) {
				const details = document.createElement('details');
				const summary = document.createElement('summary');
				summary.textContent = 'Stored secret payload';
				const code = document.createElement('pre');
				code.textContent = payload;
				details.append(summary, code);
				node.append(message(error), details);
			}
			// Do not retain ciphertext in the rendered DOM unless needed for a readable error.
			node.removeAttribute('data-secret-block');
		}
	}

	function renderCallouts(fragment: DocumentFragment): void {
		// Innermost first so nested callout bodies keep their independently rendered headers.
		for (const node of Array.from(fragment.querySelectorAll<HTMLElement>('[data-callout]')).reverse()) {
			const type = node.getAttribute('data-callout') || 'note';
			const title = node.getAttribute('data-callout-title') || calloutLabel(type);
			const foldable = node.getAttribute('data-callout-foldable') === 'true';
			const wrapper = document.createElement(foldable ? 'details' : 'div');
			wrapper.className = 'snapshot-callout';
			const group = calloutGroup(type);
			wrapper.style.setProperty('--snapshot-callout-color', `var(--callout-${typeof group === 'string' ? group : 'custom'})`);
			if (wrapper instanceof HTMLDetailsElement) wrapper.open = node.getAttribute('data-callout-folded') !== 'true';
			const header = document.createElement(foldable ? 'summary' : 'div');
			header.className = 'snapshot-callout-header';
			const icon = document.createElement('span');
			icon.setAttribute('aria-hidden', 'true');
			icon.append(purifier.sanitize(calloutIcon(type), { USE_PROFILES: { svg: true }, RETURN_DOM_FRAGMENT: true }));
			const heading = document.createElement('strong');
			heading.textContent = title;
			header.append(icon, heading);
			if (title !== calloutLabel(type)) {
				const kind = document.createElement('small');
				kind.textContent = calloutLabel(type);
				header.append(kind);
			}
			const body = document.createElement('div');
			body.className = 'snapshot-callout-body';
			while (node.firstChild) body.append(node.firstChild);
			wrapper.append(header, body);
			node.replaceWith(wrapper);
		}
	}

	function renderMath(fragment: DocumentFragment): void {
		for (const node of fragment.querySelectorAll<HTMLElement>('[data-math-inline], [data-math-block]')) {
			const block = node.hasAttribute('data-math-block');
			const stored = node.getAttribute(block ? 'data-math-block' : 'data-math-inline') || '';
			let tex = stored;
			node.className = block ? 'snapshot-math-block' : 'snapshot-math-inline';
			try {
				tex = decodeURIComponent(stored);
				const rendered = katex.renderToString(tex, { displayMode: block, trust: false, throwOnError: true });
				node.replaceChildren(purifier.sanitize(rendered, {
					USE_PROFILES: { html: true, mathMl: true, svg: true },
					FORBID_TAGS: ['style', 'a', 'img', 'image', 'foreignobject'],
					FORBID_ATTR: ['href', 'xlink:href', 'src'],
					RETURN_DOM_FRAGMENT: true,
				}));
			} catch (error) {
				const code = document.createElement('code');
				code.textContent = tex;
				const status = document.createElement('span');
				status.className = 'snapshot-message';
				status.textContent = ` Math could not be rendered: ${error instanceof Error ? error.message : String(error)}`;
				node.replaceChildren(code, status);
			}
			node.removeAttribute('data-math-inline');
			node.removeAttribute('data-math-block');
		}
	}

	function prepareDiagrams(fragment: DocumentFragment, isCurrent: () => boolean): void {
		for (const code of fragment.querySelectorAll<HTMLElement>('pre > code.language-mermaid, pre[data-language="mermaid"] > code')) {
			const source = code.textContent || '';
			const controls = document.createElement('div');
			controls.className = 'snapshot-diagram';
			const button = document.createElement('button');
			button.type = 'button';
			button.textContent = '▶ Render diagram';
			const status = document.createElement('span');
			status.className = 'snapshot-message';
			status.setAttribute('role', 'status');
			const output = document.createElement('div');
			output.className = 'snapshot-diagram-output';
			controls.append(button, status, output);
			code.parentElement!.after(controls);
			button.addEventListener('click', async () => {
				button.disabled = true;
				status.textContent = 'Rendering diagram…';
				let staging: HTMLDivElement | undefined;
				let restoreConfig: (() => void) | undefined;
				let release!: () => void;
				const previousRender = diagramQueue;
				diagramQueue = new Promise<void>((resolve) => { release = resolve; });
				await previousRender;
				try {
					const { default: mermaid } = await import('mermaid');
					if (!isCurrent() || !controls.isConnected) return;
					const previousConfig = mermaid.mermaidAPI.getConfig();
					restoreConfig = () => mermaid.initialize(previousConfig);
					mermaid.initialize({
						startOnLoad: false,
						securityLevel: 'strict',
						suppressErrorRendering: true,
						theme: document.documentElement.classList.contains('dark') ? 'dark' : 'default',
						fontFamily: 'inherit',
						htmlLabels: false,
						flowchart: { htmlLabels: false },
						secure: [
							...(mermaid.mermaidAPI.defaultConfig.secure || []),
							'htmlLabels', 'flowchart', 'themeCSS', 'themeVariables', 'fontFamily', 'altFontFamily', 'dompurifyConfig',
						],
					});
					const parsed = await mermaid.mermaidAPI.getDiagramFromText(source);
					// Strict Mermaid still fetches image-shape URLs during layout, before SVG sanitization.
					// Inspect its parsed layout data rather than trying to rewrite the fence's source.
					if ('getData' in parsed.db && typeof parsed.db.getData === 'function') {
						const data: unknown = parsed.db.getData();
						if (data && typeof data === 'object' && 'nodes' in data && Array.isArray(data.nodes)
							&& data.nodes.some((node: unknown) => node && typeof node === 'object' && 'img' in node && node.img)) {
							throw new Error('Embedded Mermaid images are not loaded in snapshot previews.');
						}
					}
					if (!isCurrent() || !controls.isConnected) return;
					staging = document.createElement('div');
					staging.className = 'snapshot-diagram-staging';
					staging.setAttribute('aria-hidden', 'true');
					staging.inert = true;
					controls.append(staging);
					const { svg } = await mermaid.render(`snapshot-diagram-${++diagramId}`, source, staging);
					if (!isCurrent() || !controls.isConnected) return;
					const clean = purifier.sanitize(svg, {
						USE_PROFILES: { svg: true },
						FORBID_TAGS: ['style', 'foreignobject', 'image', 'use', 'a'],
						FORBID_ATTR: ['style', 'href', 'xlink:href'],
						RETURN_DOM_FRAGMENT: true,
					});
					const blockedContent = purifier.removed.some((entry) => 'element' in entry
						&& ['foreignobject', 'image', 'use'].includes(entry.element.nodeName.toLowerCase()));
					for (const element of clean.querySelectorAll('*')) {
						element.removeAttribute('class');
						for (const attribute of Array.from(element.attributes)) {
							if (/url\s*\(/i.test(attribute.value) && !/^url\(\s*#[\w:.-]+\s*\)$/i.test(attribute.value)) {
								element.removeAttribute(attribute.name);
							}
						}
					}
					const diagram = clean.querySelector('svg');
					if (!diagram) throw new Error('No safe diagram output was produced.');
					diagram.setAttribute('role', 'img');
					diagram.setAttribute('aria-label', 'Rendered snapshot diagram; source is shown above');
					output.replaceChildren(clean);
					status.textContent = blockedContent
						? 'Rendered with unsupported embedded content blocked. Original source remains above.'
						: 'Rendered. Original source remains above.';
					button.textContent = '↻ Render again';
				} catch (error) {
					if (isCurrent() && controls.isConnected) {
						status.textContent = `Diagram could not be rendered: ${error instanceof Error ? error.message : String(error)} Original source remains above.`;
						button.textContent = '↻ Retry';
					}
				} finally {
					staging?.remove();
					try {
						restoreConfig?.();
					} finally {
						release();
						button.disabled = false;
					}
				}
			});
		}
	}

	$effect(() => {
		const source = html;
		const target = root;
		if (!target) return;
		let active = true;
		try {
			purifier ??= createDOMPurify(window);
			if (!purifier.isSupported) throw new Error('Safe HTML rendering is unavailable in this browser.');
			const fragment = purifier.sanitize(source, {
				ALLOWED_TAGS: documentTags,
				ALLOWED_ATTR: documentAttributes,
				ADD_URI_SAFE_ATTR: documentAttributes.filter((attribute) => attribute.startsWith('data-')),
				ALLOWED_NAMESPACES: ['http://www.w3.org/1999/xhtml'],
				ALLOW_DATA_ATTR: false,
				ALLOW_ARIA_ATTR: false,
				ALLOWED_URI_REGEXP: documentUris,
				RETURN_DOM_FRAGMENT: true,
			});
			const blockedMarkup = purifier.removed.some((entry) => 'element' in entry
				&& ['script', 'style', 'iframe', 'object', 'embed', 'form', 'input', 'textarea', 'select', 'video', 'audio', 'svg', 'math']
					.includes(entry.element.nodeName.toLowerCase()));
			prepareDocument(fragment);
			renderSecrets(fragment);
			renderCallouts(fragment);
			renderMath(fragment);
			prepareDiagrams(fragment, () => active && target.isConnected);
			if (blockedMarkup) {
				const details = document.createElement('details');
				const summary = document.createElement('summary');
				summary.textContent = 'Active or unsupported embedded content was blocked. View supplied HTML as text.';
				const code = document.createElement('pre');
				code.textContent = source;
				details.append(summary, code);
				fragment.prepend(details);
			}
			target.replaceChildren(source.trim() ? fragment : message('This snapshot has no body content.'));
		} catch (error) {
			const code = document.createElement('pre');
			code.textContent = source;
			target.replaceChildren(message(`Snapshot could not be rendered: ${error instanceof Error ? error.message : String(error)} Showing supplied HTML as text.`), code);
		}
		return () => { active = false; };
	});
</script>

<section class="snapshot-document" aria-label={label}>
	<div class="snapshot-body" bind:this={root}></div>
</section>

<style>
	.snapshot-document {
		min-width: 0;
		max-width: 100%;
		padding: 16px;
		color: var(--text-primary);
		font-family: var(--editor-font-family, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif);
		font-size: var(--editor-font-size, 14px);
		line-height: var(--editor-line-height, 1.65);
		user-select: text;
		overflow-wrap: anywhere;
	}
	.snapshot-body :global(p) { margin: 0 0 0.4em; }
	.snapshot-body :global(h1) { font-size: 1.75em; margin: 1.5em 0 0.5em; }
	.snapshot-body :global(h2) { font-size: 1.4em; margin: 1.25em 0 0.5em; }
	.snapshot-body :global(h3) { font-size: 1.2em; margin: 1em 0 0.5em; }
	.snapshot-body :global(h4), .snapshot-body :global(h5), .snapshot-body :global(h6) { margin: 0.9em 0 0.4em; }
	.snapshot-body :global(h1), .snapshot-body :global(h2), .snapshot-body :global(h3) { line-height: 1.3; }
	.snapshot-body > :global(:first-child) { margin-top: 0; }
	.snapshot-body :global(strong) { font-weight: 600; }
	.snapshot-body :global(a) { color: var(--text-accent); text-decoration: underline; }
	.snapshot-body :global(blockquote) { border-left: 3px solid var(--accent); padding-left: 16px; margin: 1em 0; color: var(--text-secondary); }
	.snapshot-body :global(code), .snapshot-body :global(kbd), .snapshot-body :global(pre) { font-family: var(--font-mono, monospace); }
	.snapshot-body :global(code) { background: color-mix(in srgb, var(--accent) 8%, var(--bg-tertiary)); padding: 2px 6px; border-radius: 4px; font-size: 0.9em; }
	.snapshot-body :global(pre) { overflow-x: auto; max-width: 100%; padding: 16px; margin: 1em 0; border-radius: 8px; background: color-mix(in srgb, var(--accent) 8%, var(--bg-tertiary)); white-space: pre; }
	.snapshot-body :global(pre code) { padding: 0; background: none; font-size: 0.93em; line-height: 1.5; }
	.snapshot-body :global(img) { max-width: 100%; height: auto; object-fit: contain; }
	.snapshot-body :global(img[data-size="small"]) { max-width: 33%; }
	.snapshot-body :global(img[data-size="medium"]) { max-width: 65%; }
	.snapshot-body :global(ul), .snapshot-body :global(ol) { padding-left: 1.5em; }
	.snapshot-body :global(ul[data-type="taskList"]) { padding-left: 0; list-style: none; }
	.snapshot-body :global(.snapshot-task) { display: flex; align-items: flex-start; gap: 8px; list-style: none; }
	.snapshot-body :global(.snapshot-task > input) { flex: none; margin-top: 0.4em; accent-color: var(--accent); }
	.snapshot-body :global(.snapshot-task > div) { flex: 1; min-width: 0; }
	.snapshot-body :global(details) { margin: 1em 0; }
	.snapshot-body :global(summary) { cursor: pointer; }
	.snapshot-body :global(.snapshot-table) { max-width: 100%; overflow-x: auto; margin: 1em 0; }
	.snapshot-body :global(table) { border-collapse: collapse; min-width: 100%; }
	.snapshot-body :global(th), .snapshot-body :global(td) { border: 1px solid var(--border); padding: 6px 10px; vertical-align: top; }
	.snapshot-body :global(th) { background: var(--bg-secondary); font-weight: 600; }
	.snapshot-body :global(.snapshot-secret) { margin: 1em 0; padding: 12px; border: 1px solid var(--border); border-radius: 8px; background: color-mix(in srgb, var(--accent) 7%, var(--bg-secondary)); }
	.snapshot-body :global(.snapshot-message) { color: var(--text-secondary); font-size: 0.9em; }
	.snapshot-body :global(.snapshot-wiki-link) { color: var(--text-accent); text-decoration: underline; text-decoration-style: dotted; }
	.snapshot-body :global(.snapshot-page-break) { margin: 1em 0; border-top: 1px dashed var(--border); }
	.snapshot-body :global(.snapshot-page-break)::before { content: 'Page break'; color: var(--text-tertiary); font-size: 0.8em; }
	.snapshot-body :global(.snapshot-callout) { border: 1px solid rgba(var(--snapshot-callout-color), 0.3); border-left: 3px solid rgb(var(--snapshot-callout-color)); border-radius: 8px; background: rgba(var(--snapshot-callout-color), 0.06); margin: 1em 0; }
	.snapshot-body :global(.snapshot-callout-header) { padding: 8px 12px; color: rgb(var(--snapshot-callout-color)); }
	.snapshot-body :global(.snapshot-callout-header > span) { display: inline-flex; vertical-align: middle; margin-right: 8px; }
	.snapshot-body :global(.snapshot-callout-header > small) { margin-left: 8px; }
	.snapshot-body :global(.snapshot-callout-body) { padding: 8px 14px 10px; }
	.snapshot-body :global(.snapshot-math-block) { overflow-x: auto; margin: 16px 0; padding: 12px; text-align: center; }
	.snapshot-body :global(.snapshot-math-block .katex-display) { margin: 0; }
	.snapshot-body :global(.snapshot-diagram) { position: relative; margin: -0.5em 0 1em; }
	.snapshot-body :global(.snapshot-diagram button) { border: 1px solid var(--border); border-radius: 6px; padding: 7px 14px; background: var(--bg-tertiary); color: var(--text-primary); font: inherit; font-size: 0.9em; cursor: pointer; }
	.snapshot-body :global(.snapshot-diagram button:hover:not(:disabled)) { border-color: var(--accent); }
	.snapshot-body :global(.snapshot-diagram button:disabled) { opacity: 0.6; cursor: wait; }
	.snapshot-body :global(.snapshot-diagram [role="status"]) { display: block; margin-top: 6px; }
	.snapshot-body :global(.snapshot-diagram-output) { overflow-x: auto; max-width: 100%; }
	.snapshot-body :global(.snapshot-diagram-output svg) { max-width: 100%; height: auto; color: var(--text-primary); }
	/* SVG stylesheets are removed; these local rules keep untrusted diagrams legible. */
	.snapshot-body :global(.snapshot-diagram-output svg text), .snapshot-body :global(.snapshot-diagram-output svg tspan) { fill: var(--text-primary); font-family: inherit; }
	.snapshot-body :global(.snapshot-diagram-output svg path), .snapshot-body :global(.snapshot-diagram-output svg line), .snapshot-body :global(.snapshot-diagram-output svg polyline) { stroke: var(--text-secondary); fill: none; }
	.snapshot-body :global(.snapshot-diagram-output svg rect), .snapshot-body :global(.snapshot-diagram-output svg circle), .snapshot-body :global(.snapshot-diagram-output svg ellipse), .snapshot-body :global(.snapshot-diagram-output svg polygon) { stroke: var(--text-secondary); fill: var(--bg-secondary); }
	.snapshot-body :global(.snapshot-diagram-output svg marker path) { fill: var(--text-secondary); }
	.snapshot-body :global(.snapshot-diagram-staging) { position: absolute; visibility: hidden; pointer-events: none; width: 100%; }
	.snapshot-body :global(summary:focus-visible), .snapshot-body :global(button:focus-visible), .snapshot-body :global(.snapshot-table:focus-visible) { outline: 2px solid var(--accent); outline-offset: 3px; }
</style>
