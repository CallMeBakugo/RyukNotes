<script lang="ts">
	import { onMount } from 'svelte';
	import type { SourceController } from '$lib/editor/source/controller';

	let { controller, readonly = false, numbers = false, visible = true, mobile = false }: {
		controller: SourceController;
		readonly?: boolean;
		numbers?: boolean;
		visible?: boolean;
		mobile?: boolean;
	} = $props();
	let host: HTMLDivElement;
	onMount(() => {
		const detach = controller.attach(host);
		const measure = () => controller.view?.requestMeasure();
		const appearance = new MutationObserver(measure);
		appearance.observe(document.documentElement, { attributes: true, attributeFilter: ['class', 'style'] });
		document.fonts.addEventListener('loadingdone', measure);
		return () => {
			appearance.disconnect();
			document.fonts.removeEventListener('loadingdone', measure);
			detach();
		};
	});
	$effect(() => { controller.configure(readonly, numbers); });
	$effect(() => { if (visible) controller.view?.requestMeasure(); });
</script>

<div class="source-editor" class:mobile class:with-line-numbers={numbers} style:display={visible ? undefined : 'none'} bind:this={host}></div>

<style>
	.source-editor {
		width: 100%;
		height: 100%;
		min-height: 0;
		max-width: var(--editor-content-width, none);
		margin: 0 auto;
		user-select: text;
	}
	.source-editor.with-line-numbers { max-width: none; }
	.source-editor.mobile :global(.cm-editor) { font-size: var(--editor-font-size, 15px); }
	.source-editor.mobile :global(.cm-content) { padding: 8px 16px max(220px, var(--editor-scroll-past-end, 65vh)); }
</style>
