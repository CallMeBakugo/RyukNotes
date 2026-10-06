<script lang="ts">
	import { onDestroy } from 'svelte';

	let { onResize, label, value, min, max, controls }: {
		onResize: (delta: number) => void;
		label?: string;
		value?: number;
		min?: number;
		max?: number;
		controls?: string;
	} = $props();
	let active = $state(false);
	let startX = 0;
	let rafId = 0;
	let pendingDelta = 0;

	function onMouseDown(e: MouseEvent) {
		e.preventDefault();
		active = true;
		startX = e.clientX;
		document.body.classList.add('resizing');
		window.addEventListener('mousemove', onMouseMove);
		window.addEventListener('mouseup', onMouseUp);
	}

	function onMouseMove(e: MouseEvent) {
		pendingDelta += e.clientX - startX;
		startX = e.clientX;
		if (!rafId) {
			rafId = requestAnimationFrame(() => {
				onResize(pendingDelta);
				pendingDelta = 0;
				rafId = 0;
			});
		}
	}

	function stopResize(flush: boolean) {
		if (active) document.body.classList.remove('resizing');
		active = false;
		if (rafId) { cancelAnimationFrame(rafId); rafId = 0; }
		if (flush && pendingDelta) onResize(pendingDelta);
		pendingDelta = 0;
		window.removeEventListener('mousemove', onMouseMove);
		window.removeEventListener('mouseup', onMouseUp);
	}

	function onMouseUp() { stopResize(true); }

	function onKeyDown(event: KeyboardEvent) {
		if (value === undefined || (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight')) return;
		event.preventDefault();
		onResize((event.key === 'ArrowLeft' ? -1 : 1) * (event.shiftKey ? 32 : 16));
	}

	onDestroy(() => stopResize(false));
</script>

<!-- A separator with a supplied range is an interactive resize control. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="resize-handle" class:active onmousedown={onMouseDown} onkeydown={onKeyDown}
	role={value === undefined ? 'presentation' : 'separator'} tabindex={value === undefined ? undefined : 0}
	aria-label={label} aria-orientation={value === undefined ? undefined : 'vertical'}
	aria-valuenow={value} aria-valuemin={min} aria-valuemax={max} aria-controls={controls}
	title={value === undefined ? undefined : 'Use Left and Right arrow keys to resize'}></div>
